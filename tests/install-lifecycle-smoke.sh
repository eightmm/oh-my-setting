#!/usr/bin/env bash
set -euo pipefail

# Real install -> update -> uninstall contract, reusable from every hosted OS.
# The fixture repository is built from the working tree so CI tests the patch,
# including uncommitted files in a pull-request checkout.

if [ "${BASH_VERSINFO[0]:-0}" -lt 5 ]; then
  echo "FAIL: install-lifecycle-smoke.sh needs Bash 5 or newer (running ${BASH_VERSION:-unknown}); on macOS: brew install bash" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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

# Legacy cleanup must work without downloading or retaining a managed runtime.
test_uninstall_with_old_python() (
  local d="$TMP/legacy-python" real_python
  real_python="$(command -v python3)"
  mkdir -p "$d/bin" "$d/modules" "$d/home"
  export HOME="$d/home" XDG_CONFIG_HOME="$d/home/.config"
  export XDG_DATA_HOME="$d/home/.local/share" XDG_STATE_HOME="$d/home/.local/state"
  export OMS_PYTHON_RUNTIME_ROOT="$d/missing-runtime" OMS_TEST_REAL_PYTHON="$real_python"
  unset OMS_INSTALL_RECEIPT OMS_CODEX_CONFIG CLAUDE_CONFIG_DIR
  export CODEX_HOME="$HOME/.codex" OMS_CLAUDE_SETTINGS="$HOME/.claude/settings.json"
  "$BASH" "$ROOT/scripts/link.sh" > "$d/link.out" 2>&1 || fail "legacy link fixture failed"
  printf 'model = "legacy-user"\n[tui]\nanimations = true\n' > "$HOME/.codex/config.toml"
  "$real_python" "$ROOT/scripts/lib/codex-hud-config.py" install "$HOME/.codex/config.toml" >/dev/null
  "$real_python" "$ROOT/scripts/lib/codex-usage-config.py" install "$HOME/.codex/config.toml" >/dev/null
  cat > "$d/modules/sitecustomize.py" <<'PY_OLD'
import builtins
original_import = builtins.__import__
def old_import(name, *args, **kwargs):
    if name == "tomllib":
        raise ImportError(name)
    return original_import(name, *args, **kwargs)
builtins.__import__ = old_import
PY_OLD
  cat > "$d/bin/python3" <<'SH_OLD'
#!/usr/bin/env bash
case "$*" in
  *sys.version_info*)
    exec "$OMS_TEST_REAL_PYTHON" -c 'import sys; sys.version_info = (3, 10, 0); exec(sys.argv[1])' "${@: -1}"
    ;;
esac
exec "$OMS_TEST_REAL_PYTHON" "$@"
SH_OLD
  chmod +x "$d/bin/python3"
  # The reduced PATH must still resolve `bash` (shebangs, nested calls) to the
  # Bash 5 running this test, not the host's stock /bin/bash.
  ln -s "$BASH" "$d/bin/bash"
  export PATH="$d/bin:/usr/bin:/bin" PYTHONPATH="$d/modules"
  for script in uninstall.sh doctor.sh update.sh auto-update.sh; do
    "$BASH" "$ROOT/scripts/$script" --help > "$d/help.out" 2>&1 || fail "$script help refused old Python"
  done
  "$BASH" "$ROOT/scripts/doctor.sh" --contract > "$d/doctor.out" 2>&1 ||
    fail "doctor contract check refused old Python: $(cat "$d/doctor.out")"
  [ "$(grep -Fc 'warn: host Python is older than 3.11' "$d/doctor.out")" = 1 ] ||
    fail "doctor must report the old host once"
  "$HOME/.local/bin/oms" --help > "$d/oms-help.out" 2>&1 || fail "oms help refused old Python"
  "$HOME/.local/bin/oms" uninstall --help > "$d/oms-uninstall-help.out" 2>&1 || fail "oms uninstall help refused old Python"
  cat > "$d/bin/codex" <<'SH_CODEX'
#!/usr/bin/env bash
if [ "$1 ${2:-} ${3:-}" = 'plugin list --json' ]; then
  printf '%s\n' '{"installed":[{"pluginId":"oh-my-setting@oh-my-setting-local","installed":true}]}'
fi
exit 0
SH_CODEX
  chmod +x "$d/bin/codex"
  codex plugin list --json | python3 -c 'import json,sys; d=json.load(sys.stdin); target="oh-my-setting@oh-my-setting-local"; assert any(p.get("pluginId")==target and p.get("installed") for p in d.get("installed", []))' || fail "Codex fixture unavailable"
  local plugin_version
  plugin_version="$(cat "$ROOT/VERSION")"
  mkdir -p "$CODEX_HOME/plugins/cache/oh-my-setting-local/oh-my-setting/$plugin_version"
  OH_MY_SETTING_CODEX_PLUGIN=1 "$HOME/.local/bin/oms" doctor > "$d/doctor.out" 2>&1 || true
  grep -Fq 'warn: Codex HUD and usage TOML checks require Python 3.11+' "$d/doctor.out" ||
    fail "doctor did not reach the old-Python Codex TOML guard: $(cat "$d/doctor.out")"
  ! grep -Fq 'fail: codex HUD' "$d/doctor.out" || fail "doctor failed the old-Python Codex HUD check"
  [ "$(grep -Fc 'warn: host Python is older than 3.11' "$d/doctor.out")" = 1 ] || fail "doctor must report the old host once"
  "$HOME/.local/bin/oms" doctor --json > "$d/doctor.json" 2> "$d/doctor.err" || true
  "$real_python" -c 'import json,sys; json.load(open(sys.argv[1]))' "$d/doctor.json" || fail "doctor JSON invalid"
  "$BASH" "$ROOT/scripts/uninstall.sh" --yes --dry-run > "$d/dry-run.out" 2>&1 || fail "legacy dry-run failed"
  "$BASH" "$ROOT/scripts/uninstall.sh" --yes > "$d/uninstall.out" 2>&1 ||
    fail "legacy uninstall failed: $(cat "$d/uninstall.out")"
  [ ! -e "$HOME/.codex/AGENTS.md" ] || fail "legacy uninstall left managed links"
  ! grep -Fq 'oh-my-setting managed' "$HOME/.codex/config.toml" || fail "legacy uninstall left TOML markers"
  grep -Fq 'model = "legacy-user"' "$HOME/.codex/config.toml" || fail "legacy cleanup changed user config"
  grep -Fq 'animations = true' "$HOME/.codex/config.toml" || fail "legacy cleanup changed user TOML"
  [ ! -e "$OMS_PYTHON_RUNTIME_ROOT" ] || fail "legacy cleanup provisioned a runtime"
  echo 'legacy-python-uninstall: ok'
)
test_update_defers_live_panel_calls() (
  local fixture="$TMP/panel-update" file
  mkdir -p "$fixture/install/scripts/lib" "$fixture/home" "$fixture/project" "$fixture/shared checkout"
  cp "$ROOT/scripts/update.sh" "$fixture/install/scripts/update.sh"
  cp "$ROOT/scripts/auto-update.sh" "$fixture/install/scripts/auto-update.sh"
  for file in install-contract.sh install-lifecycle-lock.sh file-lock.sh canonical-sync.sh; do
    cp "$ROOT/scripts/lib/$file" "$fixture/install/scripts/lib/$file"
  done
  git -C "$fixture/project" init -q
  git -C "$fixture/shared checkout" init -q
  "$PYTHON" - "$fixture" "${OMS_TEST_CALL_TIMEOUT:-120}" <<'PY_PANEL'
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import sys

fixture = Path(sys.argv[1])
timeout = float(sys.argv[2])
project, shared = fixture / 'project', fixture / 'shared checkout'
library = fixture / 'install/scripts/lib/canonical-sync.sh'
env = dict(os.environ, HOME=str(fixture / 'home'),
           XDG_CONFIG_HOME=str(fixture / 'home/config'),
           OMS_INSTALL_RECEIPT=str(fixture / 'home/missing-receipt'),
           OMS_INSTALL_LIFECYCLE_LOCK=str(fixture / 'locks/install-lifecycle.lock.d'),
           OMS_DELEGATE_WORKTREE_ROOT=str(fixture / 'no-workers'))
env.pop('OMS_UPDATE_CALL_REPOS', None)
env.pop('OMS_AUTO_UPDATE_MANAGED', None)

def update(label, expected, repos=None, cwd=project, args=(), self_call=False, blocked_path=None):
    current = dict(env)
    if repos is not None:
        current['OMS_UPDATE_CALL_REPOS'] = repos
    command = ['bash', str(fixture / 'install/scripts/update.sh'), '--no-tools', *args]
    if self_call:
        command = ['bash', '-c', '. "$1"; oms_canonical_call_acquire "$2" self || exit; '
                   'shift 2; "$@"; rc=$?; oms_canonical_call_release; exit "$rc"',
                   'self-update', str(library), str(project), *command]
    result = subprocess.run(command, cwd=cwd, env=current, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=timeout)
    assert result.returncode == expected, (label, result.returncode, result.stdout)
    if expected == 76:
        if blocked_path is None:
            assert 'installation deferred: 1 live panel call(s):' in result.stdout, (label, result.stdout)
        else:
            assert 'installation deferred:' in result.stdout and str(blocked_path) in result.stdout, (label, result.stdout)
        assert 'retry:' in result.stdout, (label, result.stdout)
    else:
        # The intentionally non-Git install stops immediately after admission.
        assert 'is not a git checkout' in result.stdout, (label, result.stdout)
        assert 'installation deferred:' not in result.stdout, (label, result.stdout)
    assert not (fixture / 'locks/install-lifecycle.lock.d').exists(), label
    return result.stdout

def start_holder(repo):
    holder = subprocess.Popen(['bash', '-c', '. "$1"; oms_canonical_call_acquire "$2" live || exit; '
                               'printf "ready\\n"; read -r done; oms_canonical_call_release',
                               'panel-holder', str(library), str(repo)],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, env=env)
    try:
        assert select.select([holder.stdout], [], [], timeout)[0], 'call registration timed out'
        assert holder.stdout.readline() == 'ready\n', 'call registration failed'
    except BaseException:
        holder.terminate()
        holder.communicate(timeout=timeout)
        raise
    return holder

holder = start_holder(shared)
try:
    repos = f'{project}\n{shared}\n{shared}\n'
    output = update('shared live call', 76, repos=repos)
    assert 'live panel call(s): live' in output, output
    update('own call with unrelated shared call', 76, repos=repos, self_call=True)
    update('plain update in call checkout', 76, cwd=shared)
    update('read-only check', 1, repos=repos, args=('--check',))
    update('empty repository list', 1, repos='')
    update('invalid repository path', 1, repos=str(fixture / 'missing'))
    alias = fixture / 'shared alias'
    alias.symlink_to(shared, target_is_directory=True)
    update('physical repository deduplication', 76, repos=f'{shared}\n{alias}')
    saved_library = library.with_suffix('.saved')
    library.rename(saved_library)
    try:
        update('minimal tree without a registry or read helper', 1, cwd=fixture)
        update('registry without its read helper', 76, repos=str(shared), blocked_path=library)
    finally:
        saved_library.rename(library)
    root_live = fixture / 'install/.oms/locks/canonical-sync.d/live'
    shutil.copytree(shared / '.oms/locks/canonical-sync.d/live', root_live)
    update('install ROOT only, unrelated working directory', 76, cwd=fixture)
    shutil.rmtree(root_live)
    update('CRLF repository path', 76, repos=str(shared) + '\r\n')
    env['CDPATH'] = str(fixture)
    update('CDPATH with relative repository', 76, repos=shared.name, cwd=fixture)
    env.pop('CDPATH')
    (fixture / '-P').symlink_to(shared, target_is_directory=True)
    update('leading dash repository path', 76, repos='-P', cwd=fixture)
finally:
    holder.communicate('done\n', timeout=timeout)
assert holder.returncode == 0, holder.returncode

registry = project / '.oms/locks/canonical-sync.d'
registry.mkdir(parents=True, exist_ok=True)
entry = registry / 'fixture'
entry.mkdir()
for pid, start in [('2147483647', 'proc:dead'), ('0', 'generation'),
                   ('malformed', 'generation'), ('', 'generation')]:
    (entry / 'pid').write_text(pid + '\n')
    (entry / 'process-start').write_text(start + '\n')
    output = update('registry identity ' + repr(pid), 1 if pid == '2147483647' else 76)
    if pid != '2147483647':
        assert str(entry) in output and 'remove' in output and 'confirm' in output, output
(entry / 'pid').write_text(str(os.getpid()) + '\n')
(entry / 'process-start').write_text('')
update('ancestor with missing generation', 76)
(entry / 'process-start').write_text('reused-generation\n')
update('reused pid generation', 1)
(entry / 'pid').unlink()
update('incomplete registration', 76)
(entry / 'process-start').unlink()
entry.rmdir()
update('zero live calls', 1)
update('own ancestor entry', 1, self_call=True)
assert not (registry / 'self').exists(), 'self registration leaked'

for relative in ('.oms', '.oms/locks', '.oms/locks/canonical-sync.d'):
    checkout = fixture / ('unsafe-' + relative.replace('/', '-'))
    checkout.mkdir()
    path = checkout / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.symlink_to(fixture / 'missing-registry', target_is_directory=True)
    update('symlinked registry component ' + relative, 76, repos=str(checkout), blocked_path=path)
    path.unlink()
    path.mkdir()
    for mode in (0, 0o400, 0o100):
        path.chmod(mode)
        try:
            if not os.access(path, os.R_OK | os.X_OK):
                update('unreadable registry component ' + relative, 76, repos=str(checkout), blocked_path=path)
            else:
                print('skip permission denial probe: current user bypasses mode bits')
        finally:
            path.chmod(0o700)
    path.rmdir()
    path.write_text('unexpected file\n')
    update('non-directory registry component ' + relative, 76, repos=str(checkout), blocked_path=path)

probe_entry = fixture / 'ancestry-entry'
probe_entry.mkdir()
(probe_entry / 'process-start').write_text('generation\n')
ancestry_probe = (
    '. "$1"; '
    'oms_file_lock_process_start_token() { printf "generation\\n"; }; '
    'ps() { case "$OMS_TEST_PARENT_MODE" in '
    'empty) return ;; zero) printf "0\\n" ;; malformed) printf "invalid\\n" ;; '
    'cycle) printf "%s\\n" "$2" ;; *) printf "%s\\n" "$(($2 + 1))" ;; esac; }; '
    'oms_canonical_call_is_ancestor "$2" "$3"'
)
for pid, mode, expected in [(100, 'next', 0), (164, 'next', 0), (165, 'next', 1),
                            (164, 'empty', 1), (164, 'zero', 1),
                            (164, 'malformed', 1), (164, 'cycle', 1)]:
    (probe_entry / 'pid').write_text(str(pid) + '\n')
    result = subprocess.run(['bash', '-c', ancestry_probe, 'ancestry-probe', str(library),
                             str(probe_entry), '100'], env=dict(env, OMS_TEST_PARENT_MODE=mode),
                            timeout=timeout)
    assert result.returncode == expected, (pid, mode, result.returncode)

# Exercise the real unattended apply route from outside the install checkout.
install = fixture / 'install'
(install / '.gitignore').write_text('.oms/\nlocal/\n')
(install / 'scripts/lib/ci-query.py').write_text('print("1 success")\n')
(install / 'value').write_text('base\n')
def git(*args):
    return subprocess.check_output(['git', '-C', str(install), *args], env=env, text=True).strip()
git('init', '-q')
git('add', '.')
git('commit', '-qm', 'panel auto-update base')
base = git('rev-parse', 'HEAD')
(install / 'value').write_text('next\n')
git('commit', '-qam', 'panel auto-update target')
git('tag', 'panel-target')
git('reset', '-q', '--hard', base)
git('remote', 'add', 'origin', str(install))
receipt = fixture / 'home/install.json'
receipt.write_text(json.dumps({'schema': 2, 'source_root': str(install), 'ref': 'panel-target',
                               'commit': base, 'channel': 'main', 'version': 'fixture',
                               'installed_at': '2026-11-05T00:00:00Z', 'previous_commit': '',
                               'profile': 'minimal', 'managed_targets': [],
                               'plugin': {'name': 'fixture', 'version': '1', 'sha256': 'fixture'},
                               'components': {name: False for name in ('tools', 'claude_hooks', 'codex_plugin',
                                                                      'auto_update', 'machine_snapshot', 'slurm_snapshot')}}))
scheduled_env = dict(env, OMS_INSTALL_RECEIPT=str(receipt),
                     OH_MY_SETTING_AUTO_UPDATE_STATE=str(fixture / 'home/auto.status'),
                     OH_MY_SETTING_AUTO_UPDATE_LOG=str(fixture / 'home/auto.log'))
holder = start_holder(install)
try:
    result = subprocess.run(['bash', str(install / 'scripts/auto-update.sh'), 'apply'], cwd=fixture,
                            env=scheduled_env, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, timeout=timeout)
    assert result.returncode == 0, result.stdout
    assert 'installation deferred: 1 live panel call(s): live' in result.stdout, result.stdout
    assert 'status=skipped' in (fixture / 'home/auto.status').read_text(), result.stdout
    assert git('rev-parse', 'HEAD') == base, 'scheduled deferral changed install HEAD'
finally:
    holder.communicate('done\n', timeout=timeout)
print('panel-update-deferral: ok')
PY_PANEL
)
if [ "${1:-}" = --panel-update-only ]; then
  test_update_defers_live_panel_calls
  exit
fi
if [ "${1:-}" = --legacy-python-only ]; then
  test_uninstall_with_old_python
  exit
fi
test_uninstall_with_old_python
test_update_defers_live_panel_calls

# Count external path tools across the lifecycle. Each shim appends one short
# line, including when independent children run at the same time.
path_tool_bin="$TMP/path-tool-bin"
path_tool_log="$TMP/path-tool-spawns.log"
mkdir -p "$path_tool_bin"
: > "$path_tool_log"
export OMS_TEST_PATH_TOOL_LOG="$path_tool_log"
OMS_TEST_REAL_DIRNAME="$(command -v dirname)"
OMS_TEST_REAL_BASENAME="$(command -v basename)"
OMS_TEST_REAL_READLINK="$(command -v readlink)"
OMS_TEST_REAL_UNAME="$(command -v uname)"
export OMS_TEST_REAL_DIRNAME OMS_TEST_REAL_BASENAME
export OMS_TEST_REAL_READLINK OMS_TEST_REAL_UNAME
for tool in dirname basename readlink uname; do
  cat > "$path_tool_bin/$tool" <<'EOF_PATH_TOOL'
#!/usr/bin/env bash
tool="${0##*/}"
printf '%s\n' "$tool" >> "$OMS_TEST_PATH_TOOL_LOG"
case "$tool" in
  dirname) exec "$OMS_TEST_REAL_DIRNAME" "$@" ;;
  basename) exec "$OMS_TEST_REAL_BASENAME" "$@" ;;
  readlink) exec "$OMS_TEST_REAL_READLINK" "$@" ;;
  uname) exec "$OMS_TEST_REAL_UNAME" "$@" ;;
esac
EOF_PATH_TOOL
  chmod +x "$path_tool_bin/$tool"
done
export PATH="$path_tool_bin:$PATH"
hash -r

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
# The lifecycle fixture never downloads CPython. Bind its copied lock to the
# interpreter provided by each CI image; tool-lock-smoke separately pins the
# release lock's exact production version.
"$PYTHON" - "$upstream/tools.lock.json" <<'PY'
import json, sys
path = sys.argv[1]
row = json.load(open(path, encoding="utf-8"))
row["python"]["version"] = "%d.%d.%d" % sys.version_info[:3]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(row, handle, indent=2)
    handle.write("\n")
PY

# All package payloads below are local fixtures, not a live registry mirror.
# Exercise the explicit reproducible-install contract throughout this suite.
export OH_MY_SETTING_TOOL_LOCK="$upstream/tools.lock.json"

# Instrument the fixture helper and mark doctor processes, including the ones
# started by install/update, without relying on which Python they choose.
tool_lock_get_log="$TMP/tool-lock-get.log"
: > "$tool_lock_get_log"
export OMS_TEST_TOOL_LOCK_GET_LOG="$tool_lock_get_log"
mv "$upstream/scripts/doctor.sh" "$upstream/scripts/doctor.actual.sh"
cat > "$upstream/scripts/doctor.sh" <<'SH_DOCTOR'
#!/usr/bin/env bash
export OMS_TEST_DOCTOR_ACTIVE=1
exec bash "${BASH_SOURCE[0]%/*}/doctor.actual.sh" "$@"
SH_DOCTOR
chmod +x "$upstream/scripts/doctor.sh"
mv "$upstream/scripts/lib/tool-lock.py" "$upstream/scripts/lib/tool-lock.actual.py"
cat > "$upstream/scripts/lib/tool-lock.py" <<'PY_TOOL_LOCK'
#!/usr/bin/env python3
import os
import sys

if os.environ.get("OMS_TEST_DOCTOR_ACTIVE") == "1" and "get" in sys.argv[1:]:
    fd = os.open(os.environ["OMS_TEST_TOOL_LOCK_GET_LOG"],
                 os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, b"get\n")
    finally:
        os.close(fd)
source = os.path.join(os.path.dirname(__file__), "tool-lock.actual.py")
with open(source, "rb") as handle:
    exec(compile(handle.read(), __file__, "exec"))
PY_TOOL_LOCK
chmod +x "$upstream/scripts/lib/tool-lock.py"

# Git for Windows ships core.autocrlf=true in its system config; reproduce it
# so a clone without a pinned eol would hand CRLF scripts to bash.
printf '[core]\n\tautocrlf = true\n' > "$TMP/gitconfig-autocrlf"
export GIT_CONFIG_GLOBAL="$TMP/gitconfig-autocrlf"
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
      printf '%s\n' 'if [ "${1:-}" = -p ]; then echo 24; elif [ "${1:-}" = --version ]; then echo v24.22.0; else echo node 24.22.0; fi'
    else
      case "$cli" in
        claude) locked=2.1.226 ;;
        codex) locked=0.147.0 ;;
        agy) locked=1.1.11 ;;
        gh) locked=2.102.0 ;;
        ntn) locked=0.21.9 ;;
        uv|uvx) locked=0.12.24 ;;
      esac
      printf '%s\n' "echo '$cli $locked'"
    fi
  } > "$bin/$cli"
  chmod +x "$bin/$cli"
done
cat > "$bin/uv" <<'EOF_UV'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version) echo 'uv 0.12.24' ;;
  python) exit 0 ;;
  venv)
    target=""
    for target in "$@"; do :; done
    mkdir -p "$target/bin"
    cat > "$target/bin/python3" <<'EOF_PYTHON'
#!/usr/bin/env bash
exec "$OMS_TEST_REAL_PYTHON" "$@"
EOF_PYTHON
    chmod +x "$target/bin/python3"
    ;;
  *) exit 2 ;;
esac
EOF_UV
chmod +x "$bin/uv"
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
      printf '%s\n' 'Installed plugins:' '- oh-my-setting (enabled, 0.7.0)' \
        '      Source:     antigravity' '      Components: 1 MCP server'
    else
      printf '%s\n' 'No plugins installed.'
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
OMS_TEST_REAL_PYTHON="$(command -v "$PYTHON")"
export OMS_TEST_REAL_PYTHON

notion_data_source_id="ea343dea-4a66-4421-9653-dfc4fe68ed10"

# --plan is the install contract's preview: it must write nothing at all, and
# what it lists must be what the install then does. Names and file contents of
# the whole fixture HOME and the sandbox cron file are compared around it.
plan_snapshot() {
  {
    find "$HOME" -print | LC_ALL=C sort
    find "$HOME" -type f -exec cksum {} + | LC_ALL=C sort
    find "$HOME" -type l -exec ls -ld {} + | LC_ALL=C sort
    if [ -e "$OH_MY_SETTING_AUTO_UPDATE_CRON_FILE" ]; then cksum < "$OH_MY_SETTING_AUTO_UPDATE_CRON_FILE"; else echo no-cron; fi
    git -C "$upstream" status --porcelain --untracked-files=all
  } 2>&1
}
# The previews are extra runs, not lifecycle steps: they stay out of the
# path-tool spawn budget measured at the end of this suite.
plan_run() { (cd "$HOME" && OMS_TEST_PATH_TOOL_LOG=/dev/null "$@"); }
plan_snapshot > "$TMP/plan-before.txt"
plan_run bash "$standalone_installer" --plan > "$TMP/plan-standalone.txt" 2>&1 ||
  { cat "$TMP/plan-standalone.txt" >&2; fail "install --plan without a checkout failed"; }
grep -Fq "checkout: not-previewed: $HOME/.oh-my-setting does not exist yet" "$TMP/plan-standalone.txt" ||
  fail "a plan with no checkout to read must say the clone is not previewed: $(cat "$TMP/plan-standalone.txt")"
! grep -q '^links: ' "$TMP/plan-standalone.txt" ||
  fail "a plan with no checkout must not describe steps it cannot derive"
plan_run bash "$upstream/install.sh" --plan --notion-data-source "$notion_data_source_id" \
  > "$TMP/plan-install.txt" 2>&1 ||
  { cat "$TMP/plan-install.txt" >&2; fail "install --plan failed"; }
plan_snapshot > "$TMP/plan-after.txt"
cmp -s "$TMP/plan-before.txt" "$TMP/plan-after.txt" ||
  fail "install --plan changed the machine: $(diff "$TMP/plan-before.txt" "$TMP/plan-after.txt" | head -n 20)"
[ ! -e "$HOME/.oh-my-setting" ] || fail "install --plan created the checkout"
grep -Fq "links: change: move your existing $HOME/.codex/AGENTS.md to $HOME/.codex/AGENTS.md.backup.<timestamp>" \
  "$TMP/plan-install.txt" || fail "the plan must name the user file an install displaces: $(cat "$TMP/plan-install.txt")"
grep -Eq "^links: change: (link $HOME/.local/bin/oms -> |copy .* -> $HOME/.local/bin/oms\$)" "$TMP/plan-install.txt" ||
  fail "the plan must list the dispatcher link"
grep -Fq "claude-hooks: change: register SessionStart hook resume-hook.sh" "$TMP/plan-install.txt" ||
  fail "the plan must list the Claude hook rows the installer expects"
grep -Fq "auto-update: change: install cron trigger" "$TMP/plan-install.txt" ||
  fail "the plan must relay the auto-update trigger preview: $(grep '^auto-update' "$TMP/plan-install.txt")"
grep -Eq '^codex-plugin: skip: ' "$TMP/plan-install.txt" || fail "a step turned off must read skip"
grep -Eq '^mcp: not-previewed: ' "$TMP/plan-install.txt" ||
  fail "a step with no write-free preview must be named not-previewed"
grep -Eq '^plan: [1-9][0-9]* change\(s\), [0-9]+ already current, [1-9][0-9]* step part\(s\) not previewed; nothing was written$' \
  "$TMP/plan-install.txt" || fail "plan summary missing: $(tail -n 1 "$TMP/plan-install.txt")"
! grep -Ev '^(install plan for |[a-z-]+: (change|current|skip|not-previewed|note): |plan: )' "$TMP/plan-install.txt" ||
  fail "every plan line must be STEP: STATUS: detail"
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
(cd "$HOME" && bash "$standalone_installer" \
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

# Wrappers set PYTHONDONTWRITEBYTECODE, so the installer must leave a .pyc
# that is hash-validated against the current source.
assert_lib_pyc_current() {
  local out
  out="$("$PYTHON" - "$dest/scripts/lib/hook_state.py" <<'PY_PYC'
import importlib.util, sys
src = sys.argv[1]
pyc = importlib.util.cache_from_source(src)
with open(pyc, "rb") as handle:
    header = handle.read(16)
with open(src, "rb") as handle:
    digest = importlib.util.source_hash(handle.read())
print("ok" if header[4:8] == b"\x03\x00\x00\x00" and header[8:16] == digest else "stale")
PY_PYC
  )" || fail "$1: no compiled scripts/lib/hook_state module"
  [ "${out%$(printf '\r')}" = ok ] || fail "$1: scripts/lib/hook_state .pyc is not current"
}
assert_lib_pyc_current "install"
for f in "$dest/install.sh" "$dest/scripts/oms" "$dest"/scripts/*.sh; do
  ! LC_ALL=C grep -q "$(printf '\r')" "$f" ||
    fail "checkout under core.autocrlf=true carries CR bytes: $f"
done
[ ! -e "$HOME/.oh-my-setting-prompts" ] && [ ! -L "$HOME/.oh-my-setting-prompts" ] ||
  fail "fresh install created a retired prompt target"
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
# The plan against the install that followed it: every link or copy it listed
# exists now, and the user file it said would be displaced has its backup.
planned_targets=0
while IFS= read -r planned; do
  case "$planned" in
    "links: change: link "*) planned="${planned#links: change: link }"; planned="${planned%% -> *}" ;;
    "links: change: copy "*) planned="${planned##* -> }" ;;
    *) continue ;;
  esac
  [ -L "$planned" ] || [ -e "$planned" ] || fail "the plan listed $planned but the install did not create it"
  planned_targets=$((planned_targets + 1))
done < "$TMP/plan-install.txt"
[ "$planned_targets" -ge 5 ] || fail "the plan listed too few link targets to compare ($planned_targets)"
set -- "$HOME/.codex/AGENTS.md.backup."*
[ -e "$1" ] || fail "the plan announced a backup of the user's rules that the install did not make"
# After the install the same preview finds the links current, and an update
# plan resolves the recorded profile and still writes nothing.
plan_snapshot > "$TMP/plan-before.txt"
plan_run "$dest/scripts/update.sh" --plan > "$TMP/plan-update.txt" 2>&1 ||
  { cat "$TMP/plan-update.txt" >&2; fail "update --plan failed"; }
plan_snapshot > "$TMP/plan-after.txt"
cmp -s "$TMP/plan-before.txt" "$TMP/plan-after.txt" ||
  fail "a plan after install changed the machine: $(diff "$TMP/plan-before.txt" "$TMP/plan-after.txt" | head -n 20)"
! grep -Eq '^links: change: (link|copy|move) ' "$TMP/plan-update.txt" ||
  fail "a plan over a current install must not relink: $(grep '^links: change' "$TMP/plan-update.txt" | head -n 5)"
grep -Eq "^links: current: .* current: $HOME/.local/bin/oms\$" "$TMP/plan-update.txt" ||
  fail "a plan over a current install must report the dispatcher current: $(grep '^links' "$TMP/plan-update.txt" | head -n 5)"
grep -Fq "update plan for $dest " "$TMP/plan-update.txt" || fail "update --plan must name the checkout"
grep -Eq '^checkout: not-previewed: fetching ' "$TMP/plan-update.txt" ||
  fail "update --plan must say the fetch is not previewed"
plan_status=0
plan_run "$dest/scripts/update.sh" --plan --check > "$TMP/plan-conflict.txt" 2>&1 || plan_status=$?
[ "$plan_status" = 2 ] || fail "update --plan --check must be refused (got $plan_status)"
export PATH="$HOME/.local/bin:$PATH"
command -v python3 >/dev/null 2>&1 ||
  fail "installer did not expose a python3 command"
# The default install applies delegate grants and retains user-owned rules.
[ -f "$antigravity_settings" ] || fail "install did not write Antigravity settings"
if ! python3 - "$antigravity_settings" <<'PY'
import json
import sys

settings = sys.argv[1]
cfg = json.load(open(settings, encoding="utf-8"))
assert cfg["theme"] == "user-before", cfg
allow = cfg["permissions"]["allow"]
assert allow[:3] == ["read_file(*)", "user(existing)", "command(*)"], cfg
assert any(rule.startswith("write_file(") for rule in allow), cfg
state = json.load(open(settings + ".oh-my-setting-permissions.json", encoding="utf-8"))
assert state == {"schema": 1, "rules": allow[2:]}, state
backup = json.load(open(settings + ".bak", encoding="utf-8"))
assert backup["permissions"]["allow"] == ["read_file(*)", "user(existing)"], backup
PY
then
  grep -n -B 2 -A 8 -iE 'granting antigravity|peer permission|provider-permission|antigravity settings' \
    "$TMP/install-out.txt" >&2 || tail -n 60 "$TMP/install-out.txt" >&2
  ls -la "$HOME/.gemini/antigravity-cli/" >&2 || true
  "$dest/scripts/provider-permissions.sh" --check >&2 || true
  # The product and this test must be reading the same bytes; name both views.
  echo "test view of $antigravity_settings:" >&2
  cat "$antigravity_settings" >&2 || true
  OMS_ANTIGRAVITY_SETTINGS="$antigravity_settings" \
    "$dest/scripts/provider-permissions.sh" --print >&2 || true
  find "$TMP" \( -name "*.oh-my-setting-permissions.json" -o -name "settings.json.bak" \) >&2 || true
  # A native python that received the POSIX spelling unconverted resolves it
  # against the drive root; name any ghost tree it may have created there.
  find /c/c /c/tmp "$TMP/../c" -maxdepth 12 -name "settings.json*" 2>/dev/null >&2 || true
  fail "--peer-permissions did not track only its new consult grants"
fi
# Re-applying a satisfied profile leaves the settings bytes unchanged.
permissions_before="$(cksum < "$antigravity_settings")"
"$dest/scripts/provider-permissions.sh" --apply --profile delegate --allow-toolchains >/dev/null ||
  fail "repeated delegate permission apply failed"
[ "$permissions_before" = "$(cksum < "$antigravity_settings")" ] ||
  fail "repeated permission apply rewrote satisfied settings"
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
  "$HOME/.gemini/config/skills/oms-agent-harness:$dest/custom-skills/oms-agent-harness" \
  "$HOME/.claude/output-styles/oms-korean.md:$dest/output-styles/oms-korean.md"; do
  path="${pair%%:*}"
  source="${pair#*:}"
  oms_install_target_matches "$source" "$path" ||
    fail "managed target mismatch: $path"
done

expected_mode="$(oms_install_link_mode)"
[ "$(oms_install_receipt_field link_mode)" = "$expected_mode" ] ||
  fail "receipt did not persist $expected_mode mode"
# The receipt lists the root agy loads, and the retired one uninstall cleans.
python3 - "$(oms_install_receipt_path)" <<'PY' || fail "receipt omits the Antigravity skill roots"
import json, sys
targets = json.load(open(sys.argv[1]))["managed_targets"]
assert ".gemini/config/skills" in targets and ".gemini/antigravity/skills" in targets
PY
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
grep -Fq 'OMS_AUTO_UPDATE_MANAGED=1' "$TMP/autoupdate.cron" &&
  grep -Fq 'auto-update.sh" apply' "$TMP/autoupdate.cron" ||
  fail "default auto-update trigger must use the OMS Python runtime in apply mode"
[ ! -e "$HOME/.config/systemd/user/oh-my-setting-autoupdate.timer" ] ||
  fail "forced cron method still wrote a systemd unit"
oms list --all > "$TMP/oms-tools.txt"
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

# Upgrade an old prompt target with a displaced user directory. This uses the
# same ownership contract in the symlink and Windows/copy lifecycle lanes.
mkdir -p "$dest/prompts" "$HOME/.oh-my-setting-prompts.backup.20260914000000"
printf 'retired prompt\n' > "$dest/prompts/SOUL.md"
printf 'user prompt\n' > "$HOME/.oh-my-setting-prompts.backup.20260914000000/user.md"
oms_install_materialize "$dest/prompts" "$HOME/.oh-my-setting-prompts"
rm -rf "$dest/prompts" "$dest/scripts/lib/__pycache__"
# A no-tools update must not heal settings or create permission state.
python3 - "$antigravity_settings" <<'PY_RESET'
import json, sys
path = sys.argv[1]
row = json.load(open(path))
row['permissions']['allow'] = ['read_file(*)', 'user(existing)']
with open(path, 'w') as handle:
    json.dump(row, handle)
PY_RESET
# The normal update must refresh registered Claude integrations without auth
# probes; uninstall must refuse ambiguous account ownership before global work.
# Registration requires private ancestors; both directories belong to the fixture.
chmod 700 "$HOME" "$HOME/.claude"
"$dest/scripts/connect-services.sh" --account claude:lifecycle >/dev/null ||
  fail "Claude lifecycle registration failed"
claude_account="$HOME/.claude/accounts/lifecycle"
python3 - "$claude_account" <<'PYACCOUNT'
import json, sys
from pathlib import Path
home = Path(sys.argv[1])
(home / "history-fixture").write_text("keep account history")
sidecar = home / ".oms-claude-hooks.json"
row = json.loads(sidecar.read_text())
row["status"] = "pending"
sidecar.write_text(json.dumps(row))
PYACCOUNT
account_uninstall_status=0
(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes) > "$TMP/account-refusal.txt" 2>&1 ||
  account_uninstall_status=$?
[ "$account_uninstall_status" != 0 ] || fail "uninstall accepted pending Claude ownership"
[ -f "$XDG_CONFIG_HOME/oh-my-setting/install.json" ] || fail "account refusal lost global receipt"
grep -Fq 'skill-router.sh' "$HOME/.claude/settings.json" || fail "account refusal changed main hooks"
python3 - "$claude_account/.oms-claude-hooks.json" <<'PYACCOUNT'
import json, sys
from pathlib import Path
path = Path(sys.argv[1])
row = json.loads(path.read_text())
row["status"] = "installed"
path.write_text(json.dumps(row))
PYACCOUNT
permissions_before="$(cksum < "$antigravity_settings")"
(cd "$HOME" && "$dest/scripts/update.sh" --no-tools)
python3 - "$claude_account" "$dest" <<'PYACCOUNT'
import json, os, sys
from pathlib import Path
home, source = map(Path, sys.argv[1:])
assert os.readlink(home / "CLAUDE.md") == str(source / "rules/global-AGENTS.md")
assert json.loads((home / ".oms-claude-hooks.json").read_text())["status"] == "installed"
assert (home / "history-fixture").read_text() == "keep account history"
PYACCOUNT
[ "$permissions_before" = "$(cksum < "$antigravity_settings")" ] ||
  fail "no-tools update changed provider settings"
assert_lib_pyc_current "update"
grep -Fxq 'user prompt' "$HOME/.oh-my-setting-prompts/user.md" ||
  fail "update did not restore displaced user prompts"
[ ! -e "$HOME/.oh-my-setting-prompts/SOUL.md" ] || fail "update kept retired managed prompts"
[ ! -e "$(oms_install_marker_path "$HOME/.oh-my-setting-prompts")" ] ||
  fail "update retained prompt copy ownership"
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
    "mcp": False,
    "agy_plugin": False,
    "auto_update": False,
    "machine_snapshot": False,
    "slurm_snapshot": False,
})
row["component_modes"] = {
    "auto_update": "check",
    "agy_plugin": "0",
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
"$dest/scripts/provider-permissions.sh" --check --profile delegate --allow-toolchains >/dev/null ||
  fail "repair did not restore the delegate profile"
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
    "mcp": False,
    "agy_plugin": False,
    "auto_update": False,
    "machine_snapshot": False,
    "slurm_snapshot": False,
}, row
assert row["component_modes"] == {
    "auto_update": "check",
    "agy_plugin": "0",
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
for git_layout in directory file; do
  if [ "$git_layout" = file ]; then
    # Git writes a native path; Git Bash does not translate gitfile contents.
    git -C "$dest" init -q --separate-git-dir "$TMP/purge-git"
    [ -f "$dest/.git" ] || fail "gitfile fixture did not create a file"
    git -C "$dest" rev-parse --is-inside-work-tree >/dev/null || fail "invalid gitfile fixture"
  fi
  purge_status=0
  (cd "$HOME" && "$dest/scripts/uninstall.sh" --yes --purge) \
    >"$TMP/purge-dirty.out" 2>&1 || purge_status=$?
  [ "$purge_status" -ne 0 ] || fail "purge deleted a dirty $git_layout checkout without explicit consent"
  grep -Fq 'purge-dirty' "$TMP/purge-dirty.out" ||
    fail "dirty purge refusal did not name the override: $(cat "$TMP/purge-dirty.out")"
  [ -f "$dest/user-local.txt" ] || fail "refused dirty purge removed the user-owned file"
  [ -e "$HOME/.codex/AGENTS.md" ] ||
    fail "dirty purge refusal partially unlinked the installation"
done

(cd "$HOME" && "$dest/scripts/uninstall.sh" --yes --purge --purge-dirty)
[ -d "$claude_account" ] || fail "uninstall removed the Claude account directory"
[ "$(cat "$claude_account/history-fixture")" = "keep account history" ] || fail "uninstall changed account history"
[ ! -L "$claude_account/skills" ] || fail "uninstall kept owned Claude account links"
python3 - "$claude_account/settings.json" <<'PYACCOUNT'
import json, sys
assert json.load(open(sys.argv[1])) == {}
PYACCOUNT
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

# Relinking from a second checkout replaces the first checkout's managed
# targets without archiving them as user backups, so uninstall restores the
# user's original. User links that merely resemble the old install (an absolute
# link into the old tree at another path, a relative link) and a modified copy
# are still user data.
relink_run() {
  # The first argument is a checkout; HOME and the receipt are the sandbox's.
  # This fixture sits outside the lifecycle the path-tool budget measures.
  (
    PATH=":$PATH:"
    PATH="${PATH//:$path_tool_bin:/:}"
    PATH="${PATH#:}"
    PATH="${PATH%:}"
    export HOME="$relink_home" CODEX_HOME="$relink_home/.codex"
    export XDG_CONFIG_HOME="$relink_home/.config" XDG_CACHE_HOME="$relink_home/.cache"
    export OMS_LOCK_DIR="$relink_home/.cache/oh-my-setting/locks"
    cd "$relink_home" && bash "$1/scripts/$2" "${@:3}"
  )
}
relink_home="$TMP/home-relink"
relink_a="$TMP/relink-a"
relink_b="$TMP/relink-b"
mkdir -p "$relink_home/.codex"
relink_home="$(cd "$relink_home" && pwd -P)"
git clone -q "$upstream" "$relink_a"
git clone -q "$upstream" "$relink_b"
relink_a="$(cd "$relink_a" && pwd -P)"
relink_b="$(cd "$relink_b" && pwd -P)"
printf 'user rules\n' > "$relink_home/.codex/AGENTS.md"
relink_run "$relink_a" link.sh > "$TMP/relink-a.out" 2>&1 ||
  fail "link from the first checkout failed: $(cat "$TMP/relink-a.out")"
# Replace two of A's targets with user links, as a user who repointed them would.
# Git Bash without native symlinks makes ln -s a copy; those cases need real links.
relink_symlinks=0
ln -s "$relink_a/README.md" "$TMP/relink-probe" 2>/dev/null && [ -L "$TMP/relink-probe" ] && relink_symlinks=1
rm -f "$TMP/relink-probe"
if [ "$relink_symlinks" = 1 ]; then
  rm -f "$relink_home/.claude/CLAUDE.md" "$relink_home/.gemini/AGENTS.md"
  ln -s "$relink_a/README.md" "$relink_home/.claude/CLAUDE.md"
  printf 'relative user rules\n' > "$relink_home/.gemini/user-gemini.md"
  ln -s user-gemini.md "$relink_home/.gemini/AGENTS.md"
fi
relink_style="$relink_home/.claude/output-styles/oms-korean.md"
if [ "$(oms_install_link_mode)" = copy ]; then
  printf 'user edit\n' >> "$relink_style"
fi
relink_run "$relink_b" link.sh > "$TMP/relink-b.out" 2>&1 ||
  fail "relink from the second checkout failed: $(cat "$TMP/relink-b.out")"

# No backup of the first checkout's managed targets, anywhere under HOME.
# A function, not case inside $(...): Bash 3.2 misparses the pattern's ")".
relink_stale_backups() {
  find "$relink_home" -name '*.backup.*' -type l 2>/dev/null |
    while IFS= read -r relink_backup; do
      case "$(readlink "$relink_backup")" in
        "$relink_a"/custom-skills/*|"$relink_a"/rules/*|"$relink_a"/scripts/*|"$relink_a"/output-styles/*)
          printf '%s\n' "$relink_backup" ;;
      esac
    done
}
relink_stale="$(relink_stale_backups)"
[ -z "$relink_stale" ] || fail "relink archived the first checkout's links as backups: $relink_stale"
[ "$(find "$relink_home/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' | wc -l | tr -d ' ')" = 1 ] ||
  fail "relink must leave exactly the user's original Codex backup"
relink_match() { (. "$relink_b/scripts/lib/install-contract.sh" && oms_install_target_matches "$@"); }
relink_match "$relink_b/rules/global-AGENTS.md" "$relink_home/.codex/AGENTS.md" ||
  fail "relink did not take over the Codex rules"
# The user links are displaced to backups with their exact link text.
if [ "$relink_symlinks" = 1 ]; then
  relink_claude_backup="$(find "$relink_home/.claude" -maxdepth 1 -name 'CLAUDE.md.backup.*' -type l)"
  [ -n "$relink_claude_backup" ] && [ "$(readlink "$relink_claude_backup")" = "$relink_a/README.md" ] ||
    fail "relink did not back up a user link into the first checkout at another path"
  relink_gemini_backup="$(find "$relink_home/.gemini" -maxdepth 1 -name 'AGENTS.md.backup.*' -type l)"
  [ -n "$relink_gemini_backup" ] && [ "$(readlink "$relink_gemini_backup")" = user-gemini.md ] ||
    fail "relink did not back up a relative user link"
fi
if [ "$(oms_install_link_mode)" = copy ]; then
  relink_style_backup="$(find "$relink_home/.claude/output-styles" -maxdepth 1 -name 'oms-korean.md.backup.*')"
  [ -n "$relink_style_backup" ] && grep -Fxq 'user edit' "$relink_style_backup" ||
    fail "relink did not back up a modified copy"
else
  [ -z "$(find "$relink_home/.claude/output-styles" -maxdepth 1 -name 'oms-korean.md.backup.*')" ] ||
    fail "relink backed up the first checkout's output style link"
fi

relink_run "$relink_b" uninstall.sh --yes > "$TMP/relink-uninstall.out" 2>&1 ||
  fail "uninstall from the second checkout failed: $(cat "$TMP/relink-uninstall.out")"
[ "$(sed -n '1p' "$relink_home/.codex/AGENTS.md")" = "user rules" ] ||
  fail "uninstall did not restore the user's Codex rules after a relink"
if [ "$relink_symlinks" = 1 ]; then
  [ -L "$relink_home/.claude/CLAUDE.md" ] &&
    [ "$(readlink "$relink_home/.claude/CLAUDE.md")" = "$relink_a/README.md" ] ||
    fail "uninstall did not restore the user link into the first checkout"
  [ -L "$relink_home/.gemini/AGENTS.md" ] &&
    [ "$(readlink "$relink_home/.gemini/AGENTS.md")" = user-gemini.md ] ||
    fail "uninstall did not restore the relative user link"
fi
if [ "$(oms_install_link_mode)" = copy ]; then
  grep -Fxq 'user edit' "$relink_style" ||
    fail "uninstall did not restore the modified copy"
else
  [ ! -e "$relink_style" ] || fail "uninstall left the output style"
fi
[ -z "$(find "$relink_home" -name '*.backup.*' 2>/dev/null)" ] ||
  fail "uninstall left backups after a relink: $(find "$relink_home" -name '*.backup.*')"
# A receipt is only a claim: one naming a directory that is not an OMS
# checkout must not let a user link into it be replaced without a backup.
relink_run "$relink_a" link.sh > "$TMP/relink-c.out" 2>&1 ||
  fail "link before the receipt edit failed: $(cat "$TMP/relink-c.out")"
relink_other="$TMP/not-oms"
mkdir -p "$relink_other/rules"
printf 'not oms\n' > "$relink_other/rules/global-AGENTS.md"
relink_other="$(cd "$relink_other" && pwd -P)"
python3 - "$relink_home/.config/oh-my-setting/install.json" "$relink_other" <<'PY' || fail "receipt edit failed"
import json, sys
path, owner = sys.argv[1], sys.argv[2]
receipt = json.load(open(path, encoding="utf-8"))
receipt["source_root"] = owner
json.dump(receipt, open(path, "w", encoding="utf-8"))
PY
if [ "$relink_symlinks" = 1 ]; then
  rm -f "$relink_home/.codex/AGENTS.md"
  ln -s "$relink_other/rules/global-AGENTS.md" "$relink_home/.codex/AGENTS.md"
fi
# Relink regardless: it also rewrites the edited receipt, which uninstall checks.
relink_run "$relink_b" link.sh > "$TMP/relink-d.out" 2>&1 ||
  fail "relink after the receipt edit failed: $(cat "$TMP/relink-d.out")"
if [ "$relink_symlinks" = 1 ]; then
  relink_kept="$(find "$relink_home/.codex" -maxdepth 1 -name 'AGENTS.md.backup.*' -type l)"
  [ -n "$relink_kept" ] && [ "$(readlink "$relink_kept")" = "$relink_other/rules/global-AGENTS.md" ] ||
    fail "a receipt naming a non-OMS directory let a user link be replaced without a backup"
fi
relink_run "$relink_b" uninstall.sh --yes > "$TMP/relink-uninstall2.out" 2>&1 ||
  fail "second uninstall failed: $(cat "$TMP/relink-uninstall2.out")"

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
      printf '%s\n' 'if [ "${1:-}" = -p ]; then echo 24; elif [ "${1:-}" = --version ]; then echo v24.22.0; else echo node 24.22.0; fi'
    else
      case "$cli" in
        claude) locked=2.1.226 ;;
        codex) locked=0.147.0 ;;
        agy) locked=1.1.11 ;;
        gh) locked=2.102.0 ;;
        ntn) locked=0.21.9 ;;
        uv|uvx) locked=0.12.24 ;;
      esac
      printf '%s\n' "echo '$cli $locked'"
    fi
  } > "$tools_bin/$cli"
  chmod +x "$tools_bin/$cli"
done
cat > "$tools_bin/uv" <<'EOF_UV'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  --version) echo 'uv 0.12.24' ;;
  python) exit 0 ;;
  venv)
    target=""
    for target in "$@"; do :; done
    mkdir -p "$target/bin"
    cat > "$target/bin/python3" <<'EOF_PYTHON'
#!/usr/bin/env bash
exec "$OMS_TEST_REAL_PYTHON" "$@"
EOF_PYTHON
    chmod +x "$target/bin/python3"
    ;;
  *) exit 2 ;;
esac
EOF_UV
chmod +x "$tools_bin/uv"
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
  PATH="$tools_bin:$PATH" OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=0 \
  bash "$upstream/install.sh") > "$TMP/tools-install.txt" 2>&1 ||
  { cat "$TMP/tools-install.txt" >&2; fail "the default install must succeed"; }
grep -Fq 'install-profile: ok (core)' "$TMP/tools-install.txt" ||
  fail "the default install did not apply the core capability profile"
[ -f "$tools_home/.config/oh-my-setting/capabilities.json" ] ||
  fail "the default install did not record a capability receipt"
python3 - "$tools_home/.config/oh-my-setting/capabilities.json" <<'PY_RECEIPT'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 1, row
assert row["requested"] == ["core"], row
assert row["primary_provider"] in ("codex", "claude", "agy"), row
PY_RECEIPT
grep -Fq 'doctor: ok' "$TMP/tools-install.txt" ||
  fail "the default install did not end on a passing doctor"
# Capability semantics survive install: the runtime front door answers from
# the fresh checkout in every ownership mode, so nested scripts/lib packages
# provably arrived (spec: post-install verification).
tools_dest="$tools_home/.oh-my-setting"
(cd "$tools_home" && env -u CODEX_HOME -u OH_MY_SETTING_MCP -u OH_MY_SETTING_AGY_PLUGIN \
  HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" XDG_CACHE_HOME="$tools_home/.cache" \
  PATH="$tools_bin:$PATH" "$tools_dest/scripts/update.sh" --no-tools) > "$TMP/optout-update.txt" 2>&1 ||
  { cat "$TMP/optout-update.txt" >&2; fail "environment-clean opt-out update failed"; }
grep -Fq 'mcp: skipped (OH_MY_SETTING_MCP=0)' "$TMP/optout-update.txt" || fail "update enabled MCP after an opted-out install"
grep -Fq 'agy-plugin: skipped (OH_MY_SETTING_AGY_PLUGIN=0)' "$TMP/optout-update.txt" || fail "update enabled Antigravity after an opted-out install"
(cd "$tools_home" && HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
  "$tools_dest/scripts/oms" runtime envelope show > "$TMP/tools-runtime.json" 2>"$TMP/tools-runtime.err") ||
  fail "oms runtime must answer right after a default install: $(cat "$TMP/tools-runtime.err")"
python3 - "$TMP/tools-runtime.json" <<'PY_RUNTIME'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row.get("schema") == 2, row
PY_RUNTIME
(cd "$tools_home" && HOME="$tools_home" "$tools_dest/scripts/oms" list > "$TMP/tools-frontdoor.txt") ||
  fail "oms list must answer after a default install"
grep -Eq '^runtime( |$)' "$TMP/tools-frontdoor.txt" ||
  fail "the front-door catalog must include the runtime after install"
# The default install grants the delegate profile even without the old flag.
HOME="$tools_home" XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
  "$tools_dest/scripts/provider-permissions.sh" --check --profile delegate --allow-toolchains >/dev/null ||
  fail "default install did not grant the delegate profile"
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
# Keep tool-integrity checks outside the caller's adopted project state.
(
  cd "$tools_home" || exit 1
  HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
    XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
    OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
    bash "$upstream/scripts/doctor.sh" > "$TMP/tool-shadow-doctor.txt" 2>&1 || true
)
grep -Fq 'tool version drift: codex (PATH command is not the managed npm package)' \
  "$TMP/tool-shadow-doctor.txt" ||
  fail "doctor accepted a same-version PATH shadow as the managed Codex package"
mv "$TMP/managed-codex-shim" "$tools_home/.local/bin/codex"

native_alias="$($PYTHON - "$upstream/tools.lock.json" <<'PY'
import json, platform, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
os_name = {"Darwin": "darwin", "Linux": "linux"}[platform.system()]
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
(
  cd "$tools_home" || exit 1
  HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
    XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
    OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
    bash "$upstream/scripts/doctor.sh" > "$TMP/native-drift-doctor.txt" 2>&1 || true
)
grep -Fq 'platform-native payload does not match the lock' \
  "$TMP/native-drift-doctor.txt" ||
  fail "doctor accepted a mismatched Codex native payload manifest"
mv "$TMP/native-manifest.backup" "$native_manifest"

transaction_marker="$npm_prefix/lib/node_modules/@openai/codex.oh-my-setting-transaction"
printf 'schema=1\n' > "$transaction_marker"
(
  cd "$tools_home" || exit 1
  HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
    XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
    OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
    bash "$upstream/scripts/doctor.sh" > "$TMP/npm-residue-doctor.txt" 2>&1 || true
)
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
(
  cd "$tools_home" || exit 1
  HOME="$tools_home" XDG_CONFIG_HOME="$tools_home/.config" \
    XDG_CACHE_HOME="$tools_home/.cache" PATH="$tools_bin:$PATH" \
    OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=0 \
    bash "$upstream/scripts/doctor.sh" > "$TMP/direct-digest-doctor.txt" 2>&1 || true
)
grep -Fq 'tool version drift: uv (managed binary digest changed' \
  "$TMP/direct-digest-doctor.txt" ||
  fail "doctor accepted a changed managed direct-tool binary"
sed -i.bak '$d' "$tools_bin/uv"
rm -f "$tools_bin/uv.bak" "$tools_bin/uv.oh-my-setting-managed"

if [ -s "$tool_lock_get_log" ]; then
  fail "doctor called tool-lock.py get $(wc -l < "$tool_lock_get_log" | tr -d ' ') times"
fi
# Each spawn costs about 25ms on windows-latest, where this suite ran 384s.
# 3905 dirname/basename/readlink/uname starts on 2026-10-01; 1220 after the
# builtins. readlink stays: ownership compares a link's exact text, which no
# builtin reads. Raise this only with a measured reason.
OMS_LIFECYCLE_PATH_TOOL_BUDGET=1400
path_tool_total="$(wc -l < "$path_tool_log" | tr -d ' ')"
if [ "$path_tool_total" -gt "$OMS_LIFECYCLE_PATH_TOOL_BUDGET" ]; then
  awk '{ count[$0]++ } END {
    for (tool in count) printf "%s: %d\n", tool, count[tool]
  }' "$path_tool_log" >&2
  fail "path-tool spawns $path_tool_total exceed budget $OMS_LIFECYCLE_PATH_TOOL_BUDGET"
fi
echo "install-lifecycle: ok ($expected_mode; path-tool spawns $path_tool_total/$OMS_LIFECYCLE_PATH_TOOL_BUDGET; tool-lock get 0)"
