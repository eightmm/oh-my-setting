#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-platform.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
# Normalize away the "//" a trailing-slash TMPDIR leaves in the template.
TMP="$(cd "$TMP" && pwd -P)"

# setup-python exposes `python.exe` but not every Windows image also exposes a
# `python3` command to Git Bash. The product installer creates the persistent
# shim; this direct library test needs a temporary equivalent first.
if ! command -v python3 >/dev/null 2>&1 && command -v python >/dev/null 2>&1; then
  mkdir -p "$TMP/bin"
  printf '%s\n' '#!/usr/bin/env bash' 'exec python "$@"' > "$TMP/bin/python3"
  chmod +x "$TMP/bin/python3"
  export PATH="$TMP/bin:$PATH"
fi

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

copyable_python() (
  local candidate probe
  probe="$(mktemp -d "$TMP/python-probe.XXXXXX")" || return 1
  trap 'rm -rf "$probe"' EXIT
  candidate="$(python3 -I -c 'import os, sys; print(os.path.realpath(sys.executable))' 2>/dev/null)" || candidate=''
  for candidate in "$candidate" /usr/bin/python3; do
    [ -n "$candidate" ] && [ -x "$candidate" ] || continue
    cp "$candidate" "$probe/python3" 2>/dev/null || continue
    printf 'home = %s\n' "${candidate%/*}" > "$probe/pyvenv.cfg"
    if "$probe/python3" -I -c 'import sys; assert sys.version_info >= (3, 11)' >/dev/null 2>&1 &&
       "$probe/python3" -I -c '' >/dev/null 2>&1; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  fail "no copyable Python 3.11 or newer found for portability fixtures"
)

assert_regular_copy() {
  local source="$1"
  local target="$2"

  [ -e "$target" ] || fail "copy is missing: $target"
  [ ! -L "$target" ] || fail "copy unexpectedly became a symlink: $target"
  diff -qr "$source" "$target" >/dev/null 2>&1 ||
    fail "copy differs from source: $target"
}

# Native Windows must stop before locks, downloads, receipts or dispatch.
test_native_windows_refusal() {
  local fixture="$TMP/native-refusal" platform entry output verb override
  local message='native Windows (Git Bash/MSYS2/Cygwin) is not supported; install inside WSL2 with the repository and home on the Linux filesystem (not /mnt/c).'
  mkdir -p "$fixture/home" "$fixture/bin"
  printf '%s\n' '#!/usr/bin/env bash' 'printf "%s\n" "$OMS_TEST_UNAME"' > "$fixture/bin/uname"
  cat > "$fixture/bin/git" <<'EOF_GIT'
#!/usr/bin/env bash
[ "${1:-}" != --version ] || exit 0
echo 'unexpected git bootstrap' >&2
exit 97
EOF_GIT
  chmod +x "$fixture/bin/uname" "$fixture/bin/git"
  cp "$ROOT/scripts/oms" "$fixture/oms"
  for platform in MINGW64_NT-10.0 MSYS_NT-10.0 CYGWIN_NT-10.0; do
    for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$fixture/oms" \
      "$ROOT/scripts/link.sh" "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh"; do
      output="$(HOME="$fixture/home" XDG_CONFIG_HOME="$fixture/home/.config" \
        OH_MY_SETTING_DIR="$fixture/home/.oh-my-setting" OMS_PLATFORM_OVERRIDE='' \
        OMS_TEST_UNAME="$platform" PATH="$fixture/bin:$PATH" \
        bash "$entry" 2>&1)" && fail "native Windows entry succeeded: $entry ($platform)"
      [ "$output" = "$message" ] || fail "wrong native Windows refusal: $output"
      [ -z "$(find "$fixture/home" -mindepth 1 -print)" ] || fail "native Windows entry wrote home"
    done
    # Directly documented tool entries refuse before reading the lock, so
    # even --help cannot reach a lock, download or provider command.
    for entry in "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
      output="$(HOME="$fixture/home" XDG_CONFIG_HOME="$fixture/home/.config" \
        OH_MY_SETTING_DIR="$fixture/home/.oh-my-setting" OMS_PLATFORM_OVERRIDE='' \
        OMS_TEST_UNAME="$platform" PATH="$fixture/bin:$PATH" \
        bash "$entry" --help 2>&1)" && fail "native Windows tool entry succeeded: $entry ($platform)"
      [ "$output" = "$message" ] || fail "wrong native Windows tool refusal: $output"
      [ -z "$(find "$fixture/home" -mindepth 1 -print)" ] || fail "native Windows tool entry wrote home"
    done
  done
  # An override can add Windows, but cannot hide either native host signal.
  for platform in MINGW64_NT-10.0 MSYS_NT-10.0 CYGWIN_NT-10.0; do
    for override in Linux linux macos 0 malformed; do
      for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$ROOT/scripts/link.sh" \
        "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh" \
        "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
        output="$(HOME="$fixture/home" OMS_PLATFORM_OVERRIDE="$override" OSTYPE=linux-gnu \
          OMS_TEST_UNAME="$platform" PATH="$fixture/bin:$PATH" \
          bash "$entry" --help 2>&1)" && fail "override hid native uname: $entry ($override)"
        [ "$output" = "$message" ] || fail "wrong conflicting uname refusal: $output"
      done
    done
  done
  for platform in msys msys2 cygwin cygwin_nt win32; do
    for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$ROOT/scripts/link.sh" \
      "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh" \
      "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
      output="$(HOME="$fixture/home" OMS_PLATFORM_OVERRIDE=Linux OSTYPE="$platform" \
        OMS_TEST_UNAME=Linux PATH="$fixture/bin:$PATH" \
        bash "$entry" --help 2>&1)" && fail "Linux signals hid native OSTYPE: $entry ($platform)"
      [ "$output" = "$message" ] || fail "wrong conflicting OSTYPE refusal: $output"
    done
  done
  (
    unset OMS_PLATFORM_OVERRIDE
    for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$ROOT/scripts/link.sh" \
      "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh" \
      "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
      OSTYPE=linux-gnu OMS_TEST_UNAME=Linux PATH="$fixture/bin:$PATH" \
        bash "$entry" --help >/dev/null || fail "Linux without override refused: $entry"
    done
  )
  for platform in '' 0 malformed; do
    for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$ROOT/scripts/link.sh" \
      "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh" \
      "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
      OMS_PLATFORM_OVERRIDE=Linux OSTYPE="$platform" OMS_TEST_UNAME=Linux PATH="$fixture/bin:$PATH" \
        bash "$entry" --help >/dev/null || fail "non-Windows OSTYPE refused: $entry ($platform)"
    done
  done
  for verb in '' help --help list update uninstall '../invalid'; do
    output="$(HOME="$fixture/home" OMS_PLATFORM_OVERRIDE=windows \
      bash "$ROOT/scripts/oms" "$verb" 2>&1)" && fail "Windows dispatcher accepted $verb"
    [ "$output" = "$message" ] || fail "wrong dispatcher refusal for $verb: $output"
  done
  # The curl/pipe installer has no platform library; re-exec must refuse too.
  output="$(HOME="$fixture/home" OMS_PLATFORM_OVERRIDE=windows OH_MY_SETTING_REEXECED=1 \
    bash < "$ROOT/install.sh" 2>&1)" && fail "standalone re-exec installer accepted Windows"
  [ "$output" = "$message" ] || fail "wrong standalone installer refusal: $output"
  [ -z "$(find "$fixture/home" -mindepth 1 -print)" ] || fail "Windows refusal wrote home"
  for platform in linux macos '' 0 malformed; do
    for entry in "$ROOT/install.sh" "$ROOT/scripts/oms" "$ROOT/scripts/link.sh" \
      "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/update.sh" \
      "$ROOT/scripts/install-tools.sh" "$ROOT/scripts/doctor.sh"; do
      OMS_PLATFORM_OVERRIDE="$platform" bash "$entry" --help >/dev/null || fail "supported help failed: $entry"
    done
  done
  echo "native-windows-refusal: ok"
}
test_native_windows_refusal

# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

test_python_security() (
  local d="$TMP/python-security" real_python version candidate
  real_python="$(copyable_python)"
  version="$("$real_python" -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])')"
  export OMS_PYTHON_RUNTIME_ROOT="$d/runtime"
  . "$ROOT/scripts/lib/python-runtime.sh"
  oms_python_runtime_prepare_root "$OMS_PYTHON_RUNTIME_ROOT"
  mkdir -p "$OMS_PYTHON_RUNTIME_ROOT/envs/$version/bin" "$OMS_PYTHON_RUNTIME_ROOT/managed"
  printf 'schema=1\nversion=%s\n' "$version" > "$OMS_PYTHON_RUNTIME_ROOT/envs/$version/.oh-my-setting-python-env"
  printf '%s\n' "$version" > "$OMS_PYTHON_RUNTIME_ROOT/current"
  cp "$real_python" "$OMS_PYTHON_RUNTIME_ROOT/envs/$version/bin/python3"
  printf 'home = %s\n' "${real_python%/*}" > "$OMS_PYTHON_RUNTIME_ROOT/envs/$version/pyvenv.cfg"
  oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" "$version" python3 bin
  oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" "$version" python3
  export PYTHONHOME="$d/foreign-home" PYTHONPATH="$d/foreign-modules"
  cat > "$d/probe-python" <<'EOF_PROBE'
#!/usr/bin/env bash
[ -z "${PYTHONHOME+x}${PYTHONPATH+x}" ] || exit 9
EOF_PROBE
  chmod +x "$d/probe-python"
  oms_python_runtime_python_matches "$d/probe-python" "$version" || fail "version probe inherited Python settings"
  oms_python_runtime_activate_fallback || fail "valid fallback rejected"
  oms_python_runtime_is_active || fail "valid runtime not active"
  [ "$PYTHONHOME" = "$d/foreign-home" ] && [ "$PYTHONPATH" = "$d/foreign-modules" ] ||
    fail "runtime selection changed caller Python settings"
  unset PYTHONHOME PYTHONPATH
  mkdir -p "$d/inject"
  printf '%s\n' 'import os' 'open(os.environ["OMS_TEST_INJECTED"], "w").close()' > "$d/inject/sitecustomize.py"
  OMS_TEST_INJECTED="$d/injected" PYTHONPATH="$d/inject" \
    "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" -c 'pass' || fail "valid launcher failed"
  [ -e "$d/injected" ] || fail "launcher stripped caller Python path"
  mkdir -p "$d/links" "$d/shadow"
  printf '%s\n' '#!/bin/sh' 'exit 1' > "$d/shadow/bash"
  chmod +x "$d/shadow/bash"
  PATH="$d/shadow:$PATH" "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" -c 'pass' ||
    fail "launcher used bash from PATH"
  printf '%s\n' '#!/bin/sh' 'exit 91' > "$d/shadow/readlink"
  cp "$d/shadow/readlink" "$d/shadow/sed"
  chmod +x "$d/shadow/readlink" "$d/shadow/sed"
  PATH="$d/shadow:$PATH" "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" -c 'pass' ||
    fail "launcher used a shadowed utility"
  cp "$real_python" "$OMS_PYTHON_RUNTIME_ROOT/managed/python3"
  ln -s "$OMS_PYTHON_RUNTIME_ROOT/managed/python3" "$d/managed-link"
  PATH="$d/shadow:$PATH" oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/managed-link" ||
    fail "shadowed readlink broke a managed interpreter link"
  ln -s "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" "$d/links/link-1"
  ln -s link-1 "$d/links/link-2"
  "$d/links/link-1" -c 'pass' || fail "absolute launcher link failed"
  "$d/links/link-2" -c 'pass' || fail "launcher link chain failed"
  ln -s "../runtime/launchers/$version/python3" "$d/links/versioned"
  "$d/links/versioned" -c 'pass' || fail "relative versioned launcher link failed"
  ln -s missing "$d/links/broken"
  ! "$d/links/broken" -c 'pass' >/dev/null 2>&1 || fail "broken launcher link accepted"
  mkdir -p "$d/fake/envs/$version/bin"
  printf 'schema=1\nowner=oh-my-setting\n' > "$d/fake/.oh-my-setting-python-runtime"
  printf 'schema=1\nversion=%s\n' "$version" > "$d/fake/envs/$version/.oh-my-setting-python-env"
  printf '%s\n' '#!/bin/sh' ': > "$OMS_TEST_FOREIGN"' > "$d/fake/envs/$version/bin/python3"
  chmod +x "$d/fake/envs/$version/bin/python3"
  ln -s "$OMS_PYTHON_RUNTIME_ROOT/bin" "$d/fake/bin"
  ln -s "$d/fake/bin/python3" "$d/links/planted"
  OMS_TEST_FOREIGN="$d/foreign-ran" "$d/links/planted" -c 'pass' || fail "planted launcher link failed"
  [ ! -e "$d/foreign-ran" ] || fail "planted launcher link ran foreign interpreter"
  printf '0.0.0\n' > "$OMS_PYTHON_RUNTIME_ROOT/current"
  local fail_at
  for fail_at in 1 2 3; do
    (
      local calls=0
      oms_python_runtime_write_launcher() { calls=$((calls + 1)); [ "$calls" -ne "$fail_at" ]; }
      if oms_python_runtime_ensure_locked /bin/true "$version"; then
        fail "ensure ignored launcher write failure $fail_at"
      fi
    )
    [ "$(cat "$OMS_PYTHON_RUNTIME_ROOT/current")" = 0.0.0 ] || fail "ensure advanced current after launcher failure $fail_at"
  done
  printf '%s\n' "$version" > "$OMS_PYTHON_RUNTIME_ROOT/current"
  local child check_launcher
  for child in bin launchers "launchers/$version"; do
    check_launcher="$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
    [ "$child" != bin ] || check_launcher="$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3"
    mv "$OMS_PYTHON_RUNTIME_ROOT/$child" "$d/displaced-child"
    ln -s "$d/displaced-child" "$OMS_PYTHON_RUNTIME_ROOT/$child"
    ! "$check_launcher" -c 'pass' >/dev/null 2>&1 || fail "launcher accepted symlinked $child"
    rm "$OMS_PYTHON_RUNTIME_ROOT/$child"
    mv "$d/displaced-child" "$OMS_PYTHON_RUNTIME_ROOT/$child"
  done
  mv "$OMS_PYTHON_RUNTIME_ROOT/envs/$version" "$d/displaced-env"
  ln -s "$d/displaced-env" "$OMS_PYTHON_RUNTIME_ROOT/envs/$version"
  ! "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" -c 'pass' >/dev/null 2>&1 || fail "launcher accepted symlinked environment"
  ! "$d/links/link-1" -c 'pass' >/dev/null 2>&1 || fail "linked launcher accepted symlinked environment"
  ! oms_python_runtime_activate_fallback || fail "fallback accepted symlinked environment"
  rm "$OMS_PYTHON_RUNTIME_ROOT/envs/$version"
  mv "$d/displaced-env" "$OMS_PYTHON_RUNTIME_ROOT/envs/$version"
  candidate="$OMS_PYTHON_RUNTIME_ROOT/envs/$version/bin/python3"
  rm "$candidate"
  ln -s "$real_python" "$candidate"
  ! oms_python_runtime_is_active || fail "active runtime accepted foreign interpreter"
  ! oms_python_runtime_activate_fallback || fail "foreign interpreter link accepted"
  ! "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" -c 'pass' >/dev/null 2>&1 || fail "launcher ran foreign interpreter"
  ! "$d/links/link-1" -c 'pass' >/dev/null 2>&1 || fail "linked launcher ran foreign interpreter"
  ! PATH="$d/shadow:$PATH" "$d/links/link-1" -c 'pass' >/dev/null 2>&1 || fail "shadowed utility allowed foreign interpreter"
  ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$OMS_PYTHON_RUNTIME_ROOT/envs" || fail "directory interpreter accepted"
  mkdir -p "$d/altbin" "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version"
  cp "$real_python" "$d/altbin/python3"
  OMS_PYTHON="$OMS_PYTHON_RUNTIME_ROOT/../altbin/python3" \
    PATH="$OMS_PYTHON_RUNTIME_ROOT/launchers/../../altbin:/usr/bin:/bin" \
    oms_python_runtime_is_active && fail "traversal interpreter accepted"
  ! oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" '3.12.9;id' python3 || fail "malformed version accepted"
  ! oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" '' python3 || fail "empty version accepted"
  ! oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" 0 python3 || fail "zero version accepted"
  ! oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" "$version" '../python3' || fail "malformed name accepted"
  cp "$ROOT/scripts/oms" "$d/copied-oms"
  "$real_python" - "$ROOT" "$d/install.json" <<'PY_RECEIPT'
import json, sys
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump({"schema": 2, "source_root": sys.argv[1]}, handle)
PY_RECEIPT
  printf '%s\n' '#!/usr/bin/env bash' ': > "$OMS_TEST_BAD_LAUNCHER"' 'exit 1' > "$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
  chmod +x "$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
  (
    export OMS_TEST_BAD_LAUNCHER="$d/bad-launcher"
    export PATH="$OMS_PYTHON_RUNTIME_ROOT/bin:/usr/bin:/bin"
    oms_python_runtime_require >/dev/null 2>&1 && fail "host fallback trusted invalid managed launcher"
    [ ! -e "$d/bad-launcher" ] || fail "host fallback ran invalid managed launcher"
  )
  (
    python3() { : > "$d/bad-function"; return 0; }
    export -f python3
    OMS_TEST_BAD_LAUNCHER="$d/bad-launcher" OMS_INSTALL_RECEIPT="$d/install.json" \
      PATH=/usr/bin:/bin bash "$d/copied-oms" list > "$d/copied.out" || fail "copied dispatcher failed"
  )
  [ ! -e "$d/bad-launcher" ] && [ ! -e "$d/bad-function" ] || fail "copied dispatcher trusted unverified Python"
  # The copied entry must execute the current owned candidate, not a stale launcher.
  local owned="$OMS_PYTHON_RUNTIME_ROOT/envs/$version" old="$OMS_PYTHON_RUNTIME_ROOT/envs/3.11.0"
  mkdir -p "$old/Scripts" "$d/no-host"
  printf 'schema=1\nversion=3.11.0\n' > "$old/.oh-my-setting-python-env"
  printf '%s\n' '#!/usr/bin/env bash' 'printf "%s\\n" "$@" > "$OMS_TEST_OLD"' 'exec "$OMS_TEST_REAL" "$@"' > "$old/Scripts/python.exe"
  printf '%s\n' '#!/usr/bin/env bash' 'exec "$OMS_TEST_OLD_EXE" "$@"' > "$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
  printf '%s\n' '#!/usr/bin/env bash' 'exit 97' > "$d/no-host/python3"
  rm "$owned/bin/python3"
  printf '%s\n' '#!/usr/bin/env bash' \
    'if [ "${1:-}" = -I ]; then' \
    '  [ -z "${PYTHONHOME+x}${PYTHONPATH+x}" ] || exit 99' \
    '  case "${2:-}" in -) : > "$OMS_TEST_DECODER" ;; -c) : > "$OMS_TEST_FLOOR" ;; *) exit 98 ;; esac' \
    'else : > "$OMS_TEST_CHILD"; fi' \
    'exec "$OMS_TEST_REAL" "$@"' > "$owned/bin/python3"
  chmod +x "$old/Scripts/python.exe" "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" "$d/no-host/python3" "$owned/bin/python3"
  export OMS_TEST_OLD="$d/old-ran" OMS_TEST_OLD_EXE="$old/Scripts/python.exe"
  export OMS_TEST_DECODER="$d/decoder-ran" OMS_TEST_FLOOR="$d/floor-ran" OMS_TEST_CHILD="$d/child-ran" OMS_TEST_REAL="$real_python"
  cp "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" "$d/launcher.before"
  cp "$OMS_PYTHON_RUNTIME_ROOT/current" "$d/current.before"
  cp "$d/install.json" "$d/receipt.before"
  cp "$owned/.oh-my-setting-python-env" "$d/env-marker.before"
  mv "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3" "$d/version-launcher"
  for candidate in direct versioned python; do
    case "$candidate" in
      versioned) cp "$d/version-launcher" "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3" ;;
      python) mv "$owned/bin/python3" "$owned/bin/python" ;;
    esac
    rm -f "$d/decoder-ran" "$d/floor-ran" "$d/old-ran" "$d/child-ran"
    OMS_INSTALL_RECEIPT="$d/install.json" PYTHONHOME=foreign PYTHONPATH=foreign \
      PATH="$d/no-host:/usr/bin:/bin" bash "$d/copied-oms" list > "$d/bound.out" || fail "copied dispatcher failed with $candidate"
    [ -e "$d/decoder-ran" ] && [ -e "$d/floor-ran" ] && [ ! -e "$d/old-ran" ] || fail "copied dispatcher binding failed for $candidate"
    grep -Fq 'doctor' "$d/bound.out" || fail "copied dispatcher omitted list entry"
    (
      export PATH="$OMS_PYTHON_RUNTIME_ROOT/bin:$d/no-host:/usr/bin:/bin" OMS_PYTHON="$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
      oms_python_runtime_require || fail "managed activation failed for $candidate"
      oms_python_runtime_is_active || fail "managed selection inactive for $candidate"
      case "$candidate" in
        direct) [ "$(command -v python3)" = "$owned/bin/python3" ] || fail "wrong direct child" ;;
        *) [ "$(command -v python3)" = "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3" ] || fail "wrong versioned child" ;;
      esac
      python3 -c 'print("bound-child")' > "$d/child.out" || fail "child failed"
      grep -Fxq bound-child "$d/child.out" && [ -e "$d/child-ran" ] || fail "child did not use current interpreter"
      [ ! -e "$d/old-ran" ] || fail "stale launcher executed"
    )
  done
  rm "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3"
  (
    export PATH="$d/no-host:/usr/bin:/bin" OMS_PYTHON="$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
    ! oms_python_runtime_require > "$d/refuse.out" 2> "$d/refuse.err" || fail "python-only layout activated without launcher"
    grep -Fq 'error: OMS requires Python 3.11+' "$d/refuse.err" || fail "python-only refusal missing diagnostic"
    [ ! -e "$d/old-ran" ] || fail "python-only refusal executed stale launcher"
  )
  (
    export PATH=/usr/bin:/bin OMS_PYTHON="$OMS_PYTHON_RUNTIME_ROOT/bin/python3"
    oms_python_runtime_require || fail "python-only layout blocked host fallback"
    [ -z "${OMS_PYTHON+x}" ] || fail "host fallback retained managed selection"
  )
  cp "$owned/bin/python" "$OMS_PYTHON_RUNTIME_ROOT/managed/bound-python"
  ln -s "$OMS_PYTHON_RUNTIME_ROOT/managed/bound-python" "$owned/bin/python3"
  (
    export PATH="$d/no-host:/usr/bin:/bin"
    oms_python_runtime_require || fail "managed symlink candidate refused"
    oms_python_runtime_is_active || fail "managed symlink selection inactive"
    [ "$(command -v python3)" = "$owned/bin/python3" ] || fail "managed symlink child not selected"
    python3 -c 'print("bound-link")' > "$d/link-child.out" || fail "managed symlink child failed"
    grep -Fxq bound-link "$d/link-child.out" || fail "managed symlink child output missing"
  )
  rm "$owned/bin/python3"
  cp "$d/version-launcher" "$OMS_PYTHON_RUNTIME_ROOT/launchers/$version/python3"
  cmp -s "$owned/.oh-my-setting-python-env" "$d/env-marker.before" || fail "bootstrap changed environment marker"
  rm "$owned/bin/python"
  printf '%s\n' 3.11.0 > "$OMS_PYTHON_RUNTIME_ROOT/current"
  rm -f "$d/old-ran"
  OMS_INSTALL_RECEIPT="$d/install.json" PATH=/usr/bin:/bin \
    bash "$d/copied-oms" list > "$d/windows-fallback.out" || fail "Windows-only layout blocked host fallback"
  [ ! -e "$d/old-ran" ] || fail "Windows-only layout executed legacy interpreter"
  if OMS_INSTALL_RECEIPT="$d/install.json" PATH="$d/no-host:/usr/bin:/bin" \
    bash "$d/copied-oms" list > "$d/windows.out" 2> "$d/windows.err"; then
    fail "Windows-only layout accepted without host Python"
  fi
  grep -Fq 'no usable install receipt' "$d/windows.err" || fail "missing receipt recovery diagnostic"
  [ ! -e "$d/old-ran" ] || fail "Windows-only layout executed interpreter without host"
  [ "$(cat "$OMS_PYTHON_RUNTIME_ROOT/current")" = 3.11.0 ] || fail "bootstrap changed current"
  printf '%s\n' "$version" > "$OMS_PYTHON_RUNTIME_ROOT/current"
  cmp -s "$OMS_PYTHON_RUNTIME_ROOT/current" "$d/current.before" && \
    cmp -s "$OMS_PYTHON_RUNTIME_ROOT/bin/python3" "$d/launcher.before" && \
    cmp -s "$d/install.json" "$d/receipt.before" || fail "bootstrap changed its inputs"
  mkdir -p "$d/old-bin"
  printf '%s\n' '#!/usr/bin/env bash' 'exit 1' > "$d/old-bin/python3"
  chmod +x "$d/old-bin/python3"
  (
    python3() { : > "$d/exported-function"; return 0; }
    export -f python3
    PATH="$d/old-bin:/usr/bin:/bin" OMS_PYTHON_RUNTIME_ROOT="$d/missing" \
      oms_python_runtime_require >/dev/null 2>&1 && fail "exported interpreter function was accepted"
    [ ! -e "$d/exported-function" ] || fail "exported interpreter function ran"
  )
  local rc
  rc=0
  printf '%s' '{"tool_input":{"command":"git reset --hard"}}' | \
    PATH="$d/old-bin:/usr/bin:/bin" OMS_PYTHON_RUNTIME_ROOT="$d/missing" \
    OMS_HARNESS_CHILD=0 OMS_PANEL_MAIN_ATTEMPT=1 OMS_PANEL_SESSION=oms-test \
    bash "$ROOT/scripts/lib/python-entry.sh" bash "$ROOT/scripts/git-move-guard-hook.sh" \
    > "$d/git.out" 2> "$d/git.err" || rc=$?
  [ "$rc" = 2 ] || fail "git guard did not fail closed"
  rc=0
  printf '{}' | PATH="$d/old-bin:/usr/bin:/bin" OMS_PYTHON_RUNTIME_ROOT="$d/missing" \
    OMS_HARNESS_CHILD=0 OH_MY_SETTING_DIR="$ROOT" bash "$ROOT/plugins/oh-my-setting/scripts/harness-hook.sh" turn-guard \
    > "$d/turn.out" 2> "$d/turn.err" || rc=$?
  [ "$rc" = 1 ] || fail "turn guard did not report unavailable"
  printf '%s' '{"tool_name":"Write","tool_input":{"file_path":"sample.py"}}' | \
    PATH="$d/old-bin:/usr/bin:/bin" OMS_PYTHON_RUNTIME_ROOT="$d/missing" \
    bash "$ROOT/scripts/syntax-guard-hook.sh" > "$d/syntax.out" 2> "$d/syntax.err" || fail "syntax hook failed"
  [ "$(grep -Fc 'error: OMS requires Python 3.11+' "$d/syntax.err")" = 1 ] || fail "syntax hook omitted runtime error"
  echo 'python-security: ok'
)
if [ "${1:-}" = --runtime-security-only ]; then
  test_python_security
  exit
fi
test_python_security

# Focused floor coverage also runs as part of the complete portability suite.
test_python_floor() (
  local d="$TMP/python-floor" real_python version out hook_command
  real_python="$(copyable_python)"
  mkdir -p "$d/bin" "$d/repo/scripts/lib"
  sed -n '/^ensure_python3() {$/,/^}$/p' "$ROOT/install.sh" > "$d/ensure.sh"
  : > "$d/repo/scripts/lib/file-lock.sh"
  cat > "$d/repo/scripts/lib/python-runtime.sh" <<'EOF'
oms_python_runtime_root() { printf '%s\n' "$HOME/runtime"; }
oms_python_runtime_bootstrap() {
  [ "${OMS_TEST_BOOTSTRAP_FAIL:-0}" != 1 ] || return 1
  local version
  version="$("$OMS_TEST_REAL_PYTHON" -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])')"
  mkdir -p "$HOME/runtime/bin" "$HOME/runtime/envs/$version/bin"
  printf 'schema=1\nversion=%s\n' "$version" > "$HOME/runtime/envs/$version/.oh-my-setting-python-env"
  printf '%s\n' "$version" > "$HOME/runtime/current"
  printf '%s\n' '#!/usr/bin/env bash' ': > "$HOME/managed-called"' \
    'exec "$OMS_TEST_REAL_PYTHON" "$@"' > "$HOME/runtime/bin/python3"
  cp "$HOME/runtime/bin/python3" "$HOME/runtime/envs/$version/bin/python3"
  chmod +x "$HOME/runtime/bin/python3" "$HOME/runtime/envs/$version/bin/python3"
  : > "$HOME/provisioned"
}
EOF
  cat > "$d/bin/python3" <<'EOF'
#!/usr/bin/env bash
if [ "${OMS_TEST_CLEAN_PROBE:-0}" = 1 ] && [ -n "${PYTHONHOME+x}${PYTHONPATH+x}" ]; then exit 9; fi
exec "$OMS_TEST_REAL_PYTHON" -c 'import sys
try:
    sys.version_info = tuple(int(n) for n in sys.argv[1].split("."))
except ValueError:
    raise SystemExit(1)
exec(sys.argv[2])' "$OMS_TEST_HOST_VERSION" "${@: -1}"
EOF
  chmod +x "$d/bin/python3"
  for version in '' 0 3.10 3.11 4.0 malformed; do
    (
      export HOME="$d/host-$version" DEST="$d/repo" PATH="$d/bin:/usr/bin:/bin"
      export OMS_TEST_REAL_PYTHON="$real_python" OMS_TEST_HOST_VERSION="$version"
      . "$d/ensure.sh"
      ensure_python3 > "$d/bootstrap.out" 2>&1 || fail "bootstrap failed for $version"
      case "$version" in
        3.11|4.0)
          [ ! -e "$HOME/provisioned" ] || fail "supported Python $version was not accepted"
          ;;
        *)
          [ -e "$HOME/provisioned" ] || fail "Python '$version' did not provision the runtime"
          [ -x "$HOME/.local/bin/python3" ] || fail "managed bootstrap did not expose its shim"
          ;;
      esac
    )
  done
  (
    export HOME="$d/clean-probe" DEST="$d/repo" PATH="$d/bin:/usr/bin:/bin"
    export OMS_TEST_REAL_PYTHON="$real_python" OMS_TEST_HOST_VERSION=3.11 OMS_TEST_CLEAN_PROBE=1
    export PYTHONHOME=foreign PYTHONPATH=foreign
    . "$d/ensure.sh"
    ensure_python3 || fail "installer probe inherited Python settings"
    [ "$PYTHONHOME" = foreign ] && [ "$PYTHONPATH" = foreign ] || fail "installer changed caller Python settings"
  )
  (
    export HOME="$d/failed-bootstrap" DEST="$d/repo" PATH="$d/bin:/usr/bin:/bin"
    export OMS_TEST_REAL_PYTHON="$real_python" OMS_TEST_HOST_VERSION=3.10 OMS_TEST_BOOTSTRAP_FAIL=1
    if bash -c 'oms_platform_is_windows() { return 1; }; . "$1"; ensure_python3' _ "$d/ensure.sh" > "$d/failure.out" 2>&1; then
      fail "bootstrap failure was accepted"
    fi
    grep -Fq "no Python 3.11+ 'python3'" "$d/failure.out" || fail "bootstrap failure omitted the required floor"
    [ ! -e "$HOME/.local/bin/python3" ] || fail "failed bootstrap left a shim"
  )
  (
    . "$ROOT/scripts/lib/python-runtime.sh"
    (
      export HOME="$d/function-host" OMS_PYTHON_RUNTIME_ROOT="$d/no-runtime" PATH="$d/bin:/usr/bin:/bin"
      export OMS_TEST_REAL_PYTHON="$real_python" OMS_TEST_HOST_VERSION=3.10
      python3() { : > "$d/function-called"; return 0; }
      export -f python3
      oms_python_runtime_require >/dev/null 2>&1 && fail "exported interpreter function was trusted"
      [ ! -e "$d/function-called" ] || fail "exported interpreter function ran"
    )
    for version in '' 0 3.10 3.11 4.0 malformed; do
      (
        export HOME="$d/resolve-$version" PATH="$d/bin:/usr/bin:/bin"
        export OMS_TEST_REAL_PYTHON="$real_python" OMS_TEST_HOST_VERSION="$version"
        export OMS_PYTHON_RUNTIME_ROOT="$HOME/runtime"
        if out="$(oms_python_runtime_require 2>&1)"; then
          case "$version" in 3.11|4.0) ;; *) fail "unsupported Python '$version' was accepted" ;; esac
        else
          case "$version" in 3.11|4.0) fail "supported Python '$version' was refused: $out" ;; esac
          [ "$out" = 'error: OMS requires Python 3.11+; reinstall to provision the managed runtime' ] ||
            fail "runtime refusal was not one clear line: $out"
        fi
      )
    done
    export HOME="$d/gui-home" OMS_TEST_REAL_PYTHON="$real_python"
    export OMS_PYTHON_RUNTIME_ROOT="$HOME/runtime"
    oms_python_runtime_prepare_root "$OMS_PYTHON_RUNTIME_ROOT"
    . "$d/repo/scripts/lib/python-runtime.sh"
    oms_python_runtime_bootstrap
    (
      . "$ROOT/scripts/lib/python-runtime.sh"
      export PATH="$d/bin:/usr/bin:/bin" OMS_TEST_HOST_VERSION=3.11
      export PYTHONHOME=bad PYTHONPATH=bad OMS_PYTHON=stale
      OMS_PYTHON_RUNTIME_ROOT="$d/missing-runtime" oms_python_runtime_require || fail "host fallback failed"
      [ "$PYTHONHOME" = bad ] && [ "$PYTHONPATH" = bad ] && [ -z "${OMS_PYTHON+x}" ] ||
        fail "host fallback changed caller Python settings or retained managed state"
    )
    mkdir -p "$OMS_PYTHON_RUNTIME_ROOT/managed/fixture/bin" "$d/links"
    cp "$real_python" "$OMS_PYTHON_RUNTIME_ROOT/managed/fixture/bin/python3"
    ln -s "$OMS_PYTHON_RUNTIME_ROOT/managed/fixture/bin/python3" "$d/links/good"
    oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/good" || fail "managed link refused"
    ln -s "$real_python" "$d/links/foreign"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/foreign" || fail "foreign link accepted"
    ln -s missing "$d/links/broken"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/broken" || fail "broken link accepted"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "" || fail "empty candidate accepted"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/0" || fail "missing zero candidate accepted"
    ln -s cycle "$d/links/cycle"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/cycle" || fail "cyclic link accepted"
    local link_target index
    link_target="$OMS_PYTHON_RUNTIME_ROOT/managed/fixture/bin/python3"
    index=1
    while [ "$index" -le 32 ]; do
      ln -s "$link_target" "$d/links/link-$index"
      link_target="$d/links/link-$index"
      index=$((index + 1))
    done
    oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$link_target" || fail "32 managed links refused"
    ln -s "$link_target" "$d/links/link-33"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/link-33" || fail "33 managed links accepted"
    ln -s /bin "$d/links/root-target"
    ! oms_python_runtime_candidate_valid "$OMS_PYTHON_RUNTIME_ROOT" "$d/links/root-target" || fail "root target accepted"
    oms_python_runtime_write_launcher "$OMS_PYTHON_RUNTIME_ROOT" 3.11.0 python3
    ! "$OMS_PYTHON_RUNTIME_ROOT/launchers/3.11.0/python3" -c 'pass' >/dev/null 2>&1 ||
      fail "launcher accepted an unowned environment"
    mkdir -p "$d/failbin"
    printf '%s\n' '#!/usr/bin/env bash' ': > "$OMS_TEST_FAKE_CALLED"' 'exit 97' > "$d/failbin/python3"
    chmod +x "$d/failbin/python3"
    export OMS_TEST_FAKE_CALLED="$d/unexpected-probe"
    (
      printf '{}' | PATH="$d/failbin:/usr/bin:/bin" OMS_SYNTAX_GUARD_HOOK=0 OMS_PYTHON_RUNTIME_ROOT="$d/missing-runtime" \
        bash "$ROOT/scripts/lib/python-entry.sh" bash "$ROOT/scripts/syntax-guard-hook.sh" ||
        fail "disabled syntax hook failed"
      [ ! -e "$d/unexpected-probe" ] || fail "disabled syntax hook probed Python"
    )
    hook_command="$("$real_python" - "$ROOT" <<'PY_HOOK'
import importlib.util, sys
from pathlib import Path
sys.dont_write_bytecode = True
root = Path(sys.argv[1])
spec = importlib.util.spec_from_file_location("hooks", root / "scripts/lib/claude-hook-surfaces.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(module.hook_command(str(root), "telemetry-hook.sh"))
PY_HOOK
)"
    export OMS_TEST_HOOK_COMMAND="$hook_command"
    PATH="$d/failbin:/usr/bin:/bin" bash -c '
      printf "%s\n" "{\"tool_name\":\"Write\",\"tool_input\":{\"file_path\":\"probe.py\"}}" |
        bash -c "$OMS_TEST_HOOK_COMMAND"
    ' _ "$ROOT" || fail "minimal-PATH hook did not reach managed Python"
    [ -e "$HOME/managed-called" ] || fail "registered hook did not use managed Python"
    rm -f "$HOME/managed-called"
    printf '{}' | OH_MY_SETTING_DIR="$ROOT" PATH="$d/failbin:/usr/bin:/bin" \
      bash "$ROOT/plugins/oh-my-setting/scripts/harness-hook.sh" telemetry-hook
    [ -e "$HOME/managed-called" ] || fail "Codex hook did not use managed Python"
    rm -f "$HOME/managed-called"
    PATH="$d/failbin:/usr/bin:/bin" bash "$ROOT/scripts/oms" list > "$d/oms-list"
    [ -e "$HOME/managed-called" ] || fail "dispatcher did not select managed Python"
    cp "$ROOT/scripts/oms" "$d/copied-oms"
    "$real_python" - "$ROOT" "$d/install.json" <<'PY_RECEIPT'
import json, sys
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump({"schema": 2, "source_root": sys.argv[1]}, handle)
PY_RECEIPT
    rm -f "$HOME/managed-called"
    PATH="$d/failbin:/usr/bin:/bin" OMS_INSTALL_RECEIPT="$d/install.json" \
      bash "$d/copied-oms" list > "$d/copied-list"
    [ -e "$HOME/managed-called" ] || fail "copied dispatcher did not use managed Python"
    if out="$(OMS_PYTHON_RUNTIME_ROOT="$d/missing-runtime" PATH="$d/failbin:/usr/bin:/bin" \
        bash -c "$hook_command" 2>&1)"; then
      fail "registered hook accepted an unavailable runtime and old system Python"
    fi
    [ "$out" = 'error: OMS requires Python 3.11+; reinstall to provision the managed runtime' ] ||
      fail "registered hook failure was not one clear line: $out"
  )
  echo 'python-floor: ok'
)
if [ "${1:-}" = --python-floor-only ]; then
  test_python_floor
  exit
fi
test_python_floor

# Both source orders must agree that a missing file fails silently.
missing_sha_file="$TMP/does-not-exist"
sha_special_supported=0
if ! oms_platform_is_windows; then
  # Windows filenames cannot contain a literal backslash.
  sha_special_supported=1
  sha_special_file="$TMP/back\\slash.txt"
  printf 'backslash path contents\n' > "$sha_special_file"
fi
for sha_source_order in common-platform platform-common; do
  sha_output="$TMP/sha-output"
  sha_error="$TMP/sha-error"
  if [ "$sha_source_order" = common-platform ]; then
    sha_sources=". '$ROOT/scripts/lib/oms-common.sh'; . '$ROOT/scripts/lib/platform.sh'"
  else
    sha_sources=". '$ROOT/scripts/lib/platform.sh'; . '$ROOT/scripts/lib/oms-common.sh'"
  fi
  sha_probe='
file_sum="$(oms_sha256_file "$1")" || exit 2
stream_sum="$(oms_sha256_stream < "$1")" || exit 3
[ "$file_sum" = "$stream_sum" ] || exit 4
printf "%s\n" "$file_sum"
'
  if bash -c "$sha_sources; oms_sha256_file \"\$1\"" _ "$missing_sha_file" > "$sha_output" 2> "$sha_error"; then
    fail "oms_sha256_file succeeded for a missing file ($sha_source_order)"
  fi
  [ ! -s "$sha_output" ] || fail "oms_sha256_file printed output for a missing file ($sha_source_order)"
  [ ! -s "$sha_error" ] || fail "oms_sha256_file printed an error for a missing file ($sha_source_order)"

  if [ "$sha_special_supported" -eq 1 ]; then
    if ! bash -c "$sha_sources; $sha_probe" _ "$sha_special_file" > "$sha_output" 2> "$sha_error"; then
      fail "oms_sha256_file did not match the stream digest for a backslash path ($sha_source_order)"
    fi
    [ "$(wc -c < "$sha_output")" -eq 65 ] || fail "oms_sha256_file did not print one 64-character digest line ($sha_source_order)"
    grep -Eq '^[0-9a-f]{64}$' "$sha_output" ||
      fail "oms_sha256_file did not print a 64-character lowercase hex digest ($sha_source_order)"
    [ ! -s "$sha_error" ] || fail "oms_sha256_file printed an error for a valid path ($sha_source_order)"
  fi
done

[ "$(OMS_PLATFORM_OVERRIDE=windows oms_platform_name)" = windows ] ||
  fail "Windows platform override was not detected"
[ "$(OMS_PLATFORM_OVERRIDE=windows oms_install_link_mode)" = copy ] ||
  fail "Windows must default to copy mode"
# Native Windows has no locked payloads: never fall back to another target.
if OMS_PLATFORM_OVERRIDE=windows oms_tool_platform > "$TMP/tool-platform.out" 2>&1; then
  fail "native Windows selected a locked tool payload: $(cat "$TMP/tool-platform.out")"
fi
grep -Fqx 'error: unsupported tool platform: native Windows' "$TMP/tool-platform.out" ||
  fail "wrong native Windows tool platform refusal: $(cat "$TMP/tool-platform.out")"
for kernel in Linux Darwin; do
  for machine in x86_64 amd64 aarch64 arm64; do
    selected="$(OMS_PLATFORM_OVERRIDE='' OMS_PLATFORM_KERNEL="$kernel" OMS_PLATFORM_MACHINE="$machine" oms_tool_platform)" ||
      fail "supported tool platform refused: $kernel $machine"
    case "$kernel:$machine:$selected" in
      Linux:x86_64:linux-amd64|Linux:amd64:linux-amd64|Linux:aarch64:linux-arm64|Linux:arm64:linux-arm64) ;;
      Darwin:x86_64:darwin-amd64|Darwin:amd64:darwin-amd64|Darwin:aarch64:darwin-arm64|Darwin:arm64:darwin-arm64) ;;
      *) fail "wrong tool platform for $kernel $machine: $selected" ;;
    esac
  done
done
for case_value in 'FreeBSD:x86_64' 'Linux:riscv64' ':x86_64' 'Linux:'; do
  if OMS_PLATFORM_OVERRIDE='' OMS_PLATFORM_KERNEL="${case_value%%:*}" OMS_PLATFORM_MACHINE="${case_value#*:}" \
      oms_tool_platform >/dev/null 2>&1; then
    fail "unsupported tool platform selected a payload: $case_value"
  fi
done
[ "$(OMS_PLATFORM_OVERRIDE=linux oms_install_link_mode)" = symlink ] ||
  fail "Linux must keep the existing symlink default"
[ "$(OMS_PLATFORM_OVERRIDE=macos oms_install_link_mode)" = symlink ] ||
  fail "macOS must keep the existing symlink default"

source_file="$TMP/source.txt"
target_file="$TMP/target.txt"
printf 'v1\n' > "$source_file"
OMS_PLATFORM_OVERRIDE=windows oms_install_materialize "$source_file" "$target_file"
assert_regular_copy "$source_file" "$target_file"
[ "$(python3 "$ROOT/scripts/lib/managed-target.py" inspect \
  "$source_file" "$target_file")" = current ] ||
  fail "managed copy inspection did not report current"
oms_install_target_matches "$source_file" "$target_file" ||
  fail "fresh file copy was not recognized"

# The source can advance between installs. The old copy remains owned while its
# content is untouched, so relinking may replace it without turning every
# previous managed version into a user backup.
printf 'v2\n' > "$source_file"
if oms_install_target_matches "$source_file" "$target_file"; then
  fail "stale file copy was reported current"
fi
[ "$(python3 "$ROOT/scripts/lib/managed-target.py" inspect \
  "$source_file" "$target_file")" = owned ] ||
  fail "managed copy inspection did not report stale ownership"
oms_install_target_owned "$source_file" "$target_file" ||
  fail "untouched stale copy lost its ownership"
oms_install_remove_managed_target "$target_file"
OMS_PLATFORM_OVERRIDE=windows oms_install_materialize "$source_file" "$target_file"
assert_regular_copy "$source_file" "$target_file"

source_dir="$TMP/source-dir"
target_dir="$TMP/target-dir"
mkdir -p "$source_dir/nested"
printf 'nested\n' > "$source_dir/nested/value.txt"
OMS_PLATFORM_OVERRIDE=windows oms_install_materialize "$source_dir" "$target_dir"
assert_regular_copy "$source_dir" "$target_dir"
oms_install_target_matches "$source_dir" "$target_dir" ||
  fail "fresh directory copy was not recognized"

# An edited managed target is user data from this point on and must no longer
# be removable as an owned copy.
printf 'user edit\n' >> "$target_dir/nested/value.txt"
[ "$(python3 "$ROOT/scripts/lib/managed-target.py" inspect \
  "$source_dir" "$target_dir")" = modified ] ||
  fail "managed copy inspection did not report a user edit"
if oms_install_target_owned "$source_dir" "$target_dir"; then
  fail "modified copy was still treated as safely removable"
fi

# A target that is simply absent is not someone else's file. Saying "foreign"
# was both untrue and expensive: answering it through the copy inspector costs
# a python3 process per probe, and doctor probes every managed target on every
# run — most of them absent on a partial install.
[ "$(oms_install_target_state "$source_file" "$TMP/never-installed")" = missing ] ||
  fail "an absent target must report missing"
[ "$(oms_install_target_mode "$source_file" "$TMP/never-installed")" = missing ] ||
  fail "an absent target must not be reported with an ownership mode"
if oms_install_target_owned "$source_file" "$TMP/never-installed"; then
  fail "an absent target must never be reported as owned"
fi

# The installer writes this shim and the uninstaller removes it, so the
# definition of "ours" has to be reachable from the platform boundary alone —
# install.sh sources nothing else when it decides whether it may replace one.
bash -c ". '$ROOT/scripts/lib/platform.sh'; command -v oms_install_python_shim_owned >/dev/null" ||
  fail "platform.sh must define the managed Python shim test"

shim="$TMP/python3"
printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
  'exec python "$@"' > "$shim"
oms_install_python_shim_owned "$shim" ||
  fail "managed Python shim was not recognized"
printf '# foreign edit\n' >> "$shim"
if oms_install_python_shim_owned "$shim"; then
  fail "modified Python shim was still treated as removable"
fi

# The uv-backed shim shape is managed too, and it must actually run: the
# interpreter is resolved through `uv python find` at call time so the shim
# survives uv relocating its managed CPython.
uv_stub_bin="$TMP/uv-stub-bin"
mkdir -p "$uv_stub_bin"
real_python3="$(command -v python3)"
cat > "$uv_stub_bin/uv" <<EOF
#!/usr/bin/env bash
[ "\$1:\$2" = "python:find" ] || exit 2
printf '%s\n' "$real_python3"
EOF
chmod +x "$uv_stub_bin/uv"
uv_shim="$TMP/uv-python3"
printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
  'exec "$(uv python find)" "$@"' > "$uv_shim"
chmod +x "$uv_shim"
oms_install_python_shim_owned "$uv_shim" ||
  fail "uv-backed managed shim was not recognized"
out="$(PATH="$uv_stub_bin:$PATH" "$uv_shim" -c 'print("uv-shim-ok")')" ||
  fail "uv-backed shim did not exec the resolved interpreter"
[ "$out" = "uv-shim-ok" ] || fail "uv-backed shim ran the wrong interpreter: $out"

# `uv python find` honors the caller's cwd (.python-version, .venv), so that
# legacy shim breaks inside an unrelated project. Repointing it at the OMS
# runtime's version-free bin/python3 must make it cwd-independent, while
# anything that is not exactly the legacy shape stays untouched.
cwd_uv_bin="$TMP/uv-cwd-bin"
mkdir -p "$cwd_uv_bin"
cat > "$cwd_uv_bin/uv" <<EOF
#!/usr/bin/env bash
[ "\$1:\$2" = "python:find" ] || exit 2
[ ! -e .python-version ] || { echo "No interpreter found for Python 3.99" >&2; exit 2; }
printf '%s\n' "$real_python3"
EOF
chmod +x "$cwd_uv_bin/uv"
cwd_project="$TMP/uv-cwd-project"
mkdir -p "$cwd_project/.venv"
printf '3.99\n' > "$cwd_project/.python-version"
if (cd "$cwd_project" && PATH="$cwd_uv_bin:$PATH" "$uv_shim" -c 'print(1)') >/dev/null 2>&1; then
  fail "legacy uv shim unexpectedly survived a project pinning another Python"
fi
bash -c ". '$ROOT/scripts/lib/platform.sh'; command -v oms_install_python_shim_repoint >/dev/null" ||
  fail "platform.sh must define the managed Python shim repoint"
repoint_root="$TMP/uv-runtime-root"
mkdir -p "$repoint_root/bin"
if oms_install_python_shim_repoint "$uv_shim" "$repoint_root"; then
  fail "shim was repointed at a runtime without an executable bin/python3"
fi
[ "$(sed -n '3p' "$uv_shim")" = 'exec "$(uv python find)" "$@"' ] ||
  fail "a missing runtime launcher must leave the legacy shim as it was"
printf '%s\n' '#!/usr/bin/env bash' "exec \"$real_python3\" -c 'print(\"runtime-ok\")'" > "$repoint_root/bin/python3"
chmod +x "$repoint_root/bin/python3"
oms_install_python_shim_repoint "$uv_shim" "$repoint_root" ||
  fail "legacy uv shim was not repointed at the runtime"
[ "$(sed -n '3p' "$uv_shim")" = "exec '$repoint_root/bin/python3' \"\$@\"" ] ||
  fail "repointed shim does not exec the runtime bin/python3"
oms_install_python_shim_owned "$uv_shim" || fail "repointed shim is no longer owned"
[ -x "$uv_shim" ] || fail "repointed shim lost its executable bit"
out="$(cd "$cwd_project" && PATH="$cwd_uv_bin:$PATH" "$uv_shim")" ||
  fail "repointed shim failed in a project pinning another Python"
[ "$out" = runtime-ok ] || fail "repointed shim ran the wrong interpreter: $out"
before="$(cat "$uv_shim")"
if oms_install_python_shim_repoint "$uv_shim" "$repoint_root"; then
  fail "an already-runtime shim must not report a repoint"
fi
[ "$(cat "$uv_shim")" = "$before" ] || fail "repoint rewrote a shim that was not the legacy uv shape"
foreign_shim="$TMP/foreign-uv-python3"
printf '%s\n' '#!/usr/bin/env bash' 'exec "$(uv python find)" "$@"' > "$foreign_shim"
before="$(cat "$foreign_shim")"
if oms_install_python_shim_repoint "$foreign_shim" "$repoint_root"; then
  fail "repoint rewrote a launcher that lacks the OMS marker"
fi
[ "$(cat "$foreign_shim")" = "$before" ] || fail "repoint modified a foreign launcher"

# Ownership only checks that line 4 is blank, so a legacy shim followed by a
# blank line and the user's own commands passes it; the rewrite must still
# leave such a file byte-identical rather than drop what the user added.
extended_shim="$TMP/extended-uv-python3"
printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
  'exec "$(uv python find)" "$@"' '' 'echo user-added-after-the-shim' > "$extended_shim"
chmod +x "$extended_shim"
cp "$extended_shim" "$TMP/extended-uv-python3.expected"
oms_install_python_shim_owned "$extended_shim" ||
  fail "fixture precondition: the extended shim must pass the line-based ownership test"
if oms_install_python_shim_repoint "$extended_shim" "$repoint_root"; then
  fail "repoint migrated a legacy shim that carries user content"
fi
cmp -s "$extended_shim" "$TMP/extended-uv-python3.expected" ||
  fail "repoint modified a legacy shim that carries user content"
printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
  'exec "$(uv python find)" "$@"' '' > "$extended_shim"
cp "$extended_shim" "$TMP/extended-uv-python3.expected"
if oms_install_python_shim_repoint "$extended_shim" "$repoint_root"; then
  fail "repoint migrated a legacy shim with a trailing blank line"
fi
cmp -s "$extended_shim" "$TMP/extended-uv-python3.expected" ||
  fail "repoint modified a legacy shim with a trailing blank line"

# The runtime root is written into executable text: a `$` or a single quote in
# it must stay literal, not expand or end the quoting.
odd_root="$TMP/odd\$root'q"
odd_shim="$TMP/odd-uv-python3"
mkdir -p "$odd_root/bin"
cp "$repoint_root/bin/python3" "$odd_root/bin/python3"
printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
  'exec "$(uv python find)" "$@"' > "$odd_shim"
chmod +x "$odd_shim"
oms_install_python_shim_repoint "$odd_shim" "$odd_root" ||
  fail "legacy uv shim was not repointed at a runtime root with \$ and a quote"
oms_install_python_shim_owned "$odd_shim" ||
  fail "shim naming a runtime root with \$ and a quote is not recognized as owned"
out="$(cd "$cwd_project" && "$odd_shim")" ||
  fail "shim naming a runtime root with \$ and a quote did not run"
[ "$out" = runtime-ok ] || fail "odd-root shim ran the wrong interpreter: $out"

# The timer's private runtime is version-scoped and never replaces user data.
runtime_repo="$TMP/runtime-repo"
runtime_source="$ROOT"
runtime_root="$TMP/python-runtime"
runtime_uv="$TMP/runtime-uv"
runtime_uv_log="$TMP/runtime-uv.log"
runtime_version="$($real_python3 -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])' | tr -d '\r')"
mkdir -p "$runtime_repo/scripts/lib"
cp "$ROOT/scripts/python-runtime.sh" "$ROOT/scripts/auto-update.sh" \
  "$ROOT/scripts/install-tools.sh" "$runtime_repo/scripts/"
for helper in python-runtime.sh tool-lock.py file-lock.sh poll.sh platform.sh; do
  cp "$ROOT/scripts/lib/$helper" "$runtime_repo/scripts/lib/"
done
"$real_python3" - "$ROOT/tools.lock.json" "$runtime_repo/tools.lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["python"]["version"] = "%d.%d.%d" % sys.version_info[:3]
with open(sys.argv[2], "w", encoding="utf-8") as handle:
    json.dump(row, handle)
PY
cat > "$runtime_uv" <<EOF_UV
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "\$*" >> "$runtime_uv_log"
printf 'env downloads=%s preference=%s managed=%s certs=%s\n' "\${UV_PYTHON_DOWNLOADS:-}" \
  "\${UV_PYTHON_PREFERENCE:-}" "\${UV_NO_MANAGED_PYTHON:-}" "\${UV_SYSTEM_CERTS:-}" >> "$runtime_uv_log"
case "\${1:-}" in
  --version) echo 'uv 0.12.24' ;;
  python) [ "\${OMS_TEST_UV_FAIL:-0}" != 1 ] ;;
  venv)
    for target in "\$@"; do :; done
    mkdir -p "\$target/bin"
    printf '%s\n' '#!/usr/bin/env bash' 'exec "$real_python3" "\$@"' > "\$target/bin/python3"
    chmod +x "\$target/bin/python3"
    ;;
  *) exit 2 ;;
esac
EOF_UV
chmod +x "$runtime_uv"
(
  export OMS_PYTHON_RUNTIME_ROOT="$runtime_root"
  # A user's project-side uv choices must not reach the private runtime build.
  export UV_PYTHON_DOWNLOADS=never UV_PYTHON_PREFERENCE=only-system UV_NO_MANAGED_PYTHON=1
  # shellcheck source=scripts/lib/file-lock.sh
  . "$ROOT/scripts/lib/file-lock.sh"
  # shellcheck source=scripts/lib/python-runtime.sh
  . "$ROOT/scripts/lib/python-runtime.sh"
  oms_python_runtime_ensure "$runtime_uv" "$runtime_version" >/dev/null &
  first_pid=$!
  oms_python_runtime_ensure "$runtime_uv" "$runtime_version" >/dev/null &
  second_pid=$!
  wait "$first_pid" && wait "$second_pid" || fail "concurrent runtime creation failed"
  [ "$(grep -c '^venv ' "$runtime_uv_log")" = 1 ] || fail "runtime builds were not serialized"
  ROOT="$runtime_repo"
  export PYTHONHOME="$TMP/nonexistent-python-home" PYTHONPATH="$TMP/foreign-modules"
  oms_python_runtime_activate || fail "private runtime activation failed"
  [ "$PYTHONHOME" = "$TMP/nonexistent-python-home" ] && [ "$PYTHONPATH" = "$TMP/foreign-modules" ] ||
    fail "activation changed caller Python settings"
  unset PYTHONHOME PYTHONPATH
  [ "$(python3 -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])' | tr -d '\r')" = "$runtime_version" ] ||
    fail "private runtime selected a different interpreter"
  case "$(command -v python3)" in "$runtime_root/launchers/$runtime_version/python3") ;; *)
    fail "private runtime did not shadow system Python" ;; esac
  (
    export HOME="$TMP/hidden-provider-home"
    node_version="$(oms_lock_scalar "$ROOT/tools.lock.json" node.version | tr -d '\r')"
    node_bin="$HOME/.nvm/versions/node/v$node_version/bin"
    mkdir -p "$HOME/.local/bin" "$node_bin"
    for provider in claude codex agy node; do
      printf '#!/bin/sh\nexit 99\n' > "$node_bin/$provider"
      chmod +x "$node_bin/$provider"
    done
    cp "$node_bin/agy" "$HOME/.local/bin/agy"
    # Like the Windows python3 shim, this interpreter needs another PATH entry.
    interpreter_bin="$TMP/hidden-interpreter-bin"
    mkdir -p "$interpreter_bin"
    printf '%s\n' '#!/usr/bin/env bash' 'exec oms_fixture_python "$@"' > "$interpreter_bin/python3"
    printf '%s\n' '#!/usr/bin/env bash' "exec \"$real_python3\" \"\$@\"" > "$interpreter_bin/oms_fixture_python"
    chmod +x "$interpreter_bin/python3" "$interpreter_bin/oms_fixture_python"
    export PATH="$node_bin:$HOME/.local/bin:$interpreter_bin:$PATH"
    printf 'nvm() { export PATH="$NVM_BIN:$PATH"; }\n' > "$HOME/.nvm/nvm.sh"
    export NVM_BIN="$node_bin" NVM_DIR="$HOME/.nvm" OMS_PANEL_DISPATCH=1 OMS_ROOM_REPO="$TMP/live-room-decoy"
    # shellcheck source=tests/lib/smoke-env.sh
    . "$runtime_source/tests/lib/smoke-env.sh"
    [ -z "${NVM_BIN:-}${NVM_DIR:-}${OMS_PANEL_DISPATCH:-}${OMS_ROOM_REPO:-}" ] ||
      fail "fixture cleanup retained host tool or panel routing"
    # Retain interpreter/tool directories, including setup-python on Windows.
    filtered_path=""
    remaining_path="$PATH:"
    while [ -n "$remaining_path" ]; do
      path_dir="${remaining_path%%:*}"
      remaining_path="${remaining_path#*:}"
      provider_dir=0
      for provider in claude codex agy; do
        for suffix in "" .exe .cmd; do
          [ ! -e "${path_dir:-.}/$provider$suffix" ] || provider_dir=1
        done
      done
      [ "$provider_dir" = 1 ] || filtered_path="${filtered_path:+$filtered_path:}$path_dir"
    done
    PATH="$filtered_path"
    export PATH
    hash -r
    [ "$(command -v python3)" = "$interpreter_bin/python3" ] || fail "provider hiding lost the Python shim"
    python3 -c 'import sys; assert sys.version_info.major == 3' || fail "provider hiding lost the shim interpreter"
    # Reintroduce contaminated manager inputs: PATH-only fixtures still win.
    export NVM_BIN="$node_bin" NVM_DIR="$HOME/.nvm"
    # shellcheck source=scripts/lib/peer-common.sh
    . "$runtime_source/scripts/lib/peer-common.sh"
    load_user_tool_paths
    for provider in claude codex agy; do
      ! command -v "$provider" >/dev/null || fail "peer paths restored hidden $provider"
    done
    oms_python_runtime_activate "$runtime_repo" || fail "provider-hidden runtime activation failed"
    for provider in claude codex agy; do
      ! command -v "$provider" >/dev/null || fail "Python activation restored hidden $provider"
    done
    python3 -c 'import sys; assert sys.version_info.major == 3' || fail "provider hiding lost managed Python"
  )
  mkdir -p "$TMP/runtime-module"
  printf 'value = 42\n' > "$TMP/runtime-module/oms_runtime_probe.py"
  PYTHONPATH="$TMP/runtime-module" python3 -c 'import oms_runtime_probe; assert oms_runtime_probe.value == 42' ||
    fail "runtime launcher stripped an explicit OMS module path"

  # Both POSIX interpreter names must work through every public entry point.
  runtime_env="$runtime_root/envs/$runtime_version"
  for spelling in python3 python; do
    if [ "$spelling" = python ]; then mv "$runtime_env/bin/python3" "$runtime_env/bin/python"; fi
    [ "$(oms_python_runtime_env_python "$runtime_env")" = "$runtime_env/bin/$spelling" ] ||
      fail "runtime selector rejected bin/$spelling"
    for launcher in "$runtime_root/launchers/$runtime_version/python3" \
      "$runtime_root/launchers/$runtime_version/python" "$runtime_root/bin/python3"; do
      [ "$("$launcher" -c 'import sys; print(sys.argv[1])' forwarded)" = forwarded ] ||
        fail "launcher rejected bin/$spelling or lost arguments: $launcher"
    done
    "$runtime_repo/scripts/python-runtime.sh" status > "$TMP/runtime-status.out" || fail "status rejected bin/$spelling"
    [ "$("$runtime_repo/scripts/python-runtime.sh" path)" = "$runtime_root/launchers/$runtime_version" ] || fail "path rejected bin/$spelling"
    [ "$("$runtime_repo/scripts/python-runtime.sh" python)" = "$runtime_env/bin/$spelling" ] || fail "python rejected bin/$spelling"
    [ "$("$runtime_repo/scripts/python-runtime.sh" run python3 -c 'import sys; print(sys.argv[1])' forwarded)" = forwarded ] || fail "run rejected bin/$spelling"
  done
  mv "$runtime_env/bin/python" "$runtime_env/bin/python3"

  # A version-valid Windows-only executable must never be selected or run.
  mv "$runtime_env/bin/python3" "$TMP/runtime-posix-python"
  mkdir -p "$runtime_env/Scripts"
  printf '%s\n' '#!/usr/bin/env bash' "echo called >> \"$TMP/windows-runtime-called\"" \
    "exec \"$real_python3\" \"\$@\"" > "$runtime_env/Scripts/python.exe"
  chmod +x "$runtime_env/Scripts/python.exe"
  oms_python_runtime_python_matches "$runtime_env/Scripts/python.exe" "$runtime_version" || fail "Windows fixture is not version-valid"
  rm "$TMP/windows-runtime-called"
  for launcher_name in python3 python; do
    oms_python_runtime_write_launcher "$runtime_root" "$runtime_version" "$launcher_name" || fail "could not generate launcher"
  done
  oms_python_runtime_write_launcher "$runtime_root" "$runtime_version" python3 bin || fail "could not generate stable launcher"
  if oms_python_runtime_env_python "$runtime_env" > "$TMP/runtime-invalid.out" 2>&1; then fail "selector accepted Windows-only runtime"; fi
  for launcher in "$runtime_root/launchers/$runtime_version/python3" \
    "$runtime_root/launchers/$runtime_version/python" "$runtime_root/bin/python3"; do
    set +e
    "$launcher" -c 'exit(0)' > "$TMP/runtime-invalid.out" 2>&1
    result=$?
    set -e
    [ "$result" = 1 ] || fail "launcher returned $result for Windows-only runtime: $launcher"
    grep -Fq 'OMS Python runtime is incomplete' "$TMP/runtime-invalid.out" || fail "launcher omitted incomplete diagnostic"
  done
  for form in status path python run; do
    set +e
    if [ "$form" = run ]; then
      "$runtime_repo/scripts/python-runtime.sh" "$form" python3 -c 'exit(0)' > "$TMP/runtime-invalid.out" 2>&1
    else
      "$runtime_repo/scripts/python-runtime.sh" "$form" > "$TMP/runtime-invalid.out" 2>&1
    fi
    result=$?
    set -e
    [ "$result" = 1 ] || fail "$form returned $result for Windows-only runtime"
    case "$form" in status) diagnostic='python-runtime: missing or invalid' ;; *) diagnostic='OMS Python runtime does not match' ;; esac
    grep -Fq "$diagnostic" "$TMP/runtime-invalid.out" || fail "$form omitted invalid diagnostic"
  done
  cp "$runtime_root/launchers/$runtime_version/python3" "$TMP/runtime-launcher3.before"
  cp "$runtime_root/launchers/$runtime_version/python" "$TMP/runtime-launcher.before"
  cp "$runtime_root/bin/python3" "$TMP/runtime-stable.before"
  cp "$runtime_uv_log" "$TMP/runtime-uv.before"
  cp "$runtime_root/current" "$TMP/runtime-current.before"
  cp "$runtime_env/.oh-my-setting-python-env" "$TMP/runtime-env.before"
  cp "$runtime_root/.oh-my-setting-python-runtime" "$TMP/runtime-root.before"
  if oms_python_runtime_ensure "$runtime_uv" "$runtime_version" > "$TMP/runtime-invalid.out" 2>&1; then fail "ensure accepted Windows-only runtime"; fi
  grep -Fq 'existing environment is invalid; preserved for repair' "$TMP/runtime-invalid.out" || fail "ensure omitted repair diagnostic"
  cmp -s "$runtime_uv_log" "$TMP/runtime-uv.before" && cmp -s "$runtime_root/current" "$TMP/runtime-current.before" &&
    cmp -s "$runtime_env/.oh-my-setting-python-env" "$TMP/runtime-env.before" &&
    cmp -s "$runtime_root/.oh-my-setting-python-runtime" "$TMP/runtime-root.before" &&
    cmp -s "$runtime_root/launchers/$runtime_version/python3" "$TMP/runtime-launcher3.before" &&
    cmp -s "$runtime_root/launchers/$runtime_version/python" "$TMP/runtime-launcher.before" &&
    cmp -s "$runtime_root/bin/python3" "$TMP/runtime-stable.before" &&
    [ -x "$runtime_env/Scripts/python.exe" ] || fail "ensure changed invalid owned runtime"
  [ ! -e "$TMP/windows-runtime-called" ] || fail "Windows-only interpreter was executed"
  mv "$TMP/runtime-posix-python" "$runtime_env/bin/python3"

  # A different checkout changing the bootstrap hint must not break rollback.
  printf '0.0.1\n' > "$runtime_root/current"
  "$runtime_repo/scripts/python-runtime.sh" run python3 -c 'import sys; assert sys.version_info.major == 3' ||
    fail "a different current pointer disabled a valid locked environment"
  if OMS_TEST_UV_FAIL=1 oms_python_runtime_ensure "$runtime_uv" 0.0.2 >/dev/null 2>&1; then
    fail "failed runtime download was accepted"
  fi
  [ -x "$runtime_root/envs/$runtime_version/bin/python3" ] || fail "failed upgrade removed the previous runtime"
  mkdir -p "$runtime_root/envs/0.0.3.oh-my-setting-stage"
  printf 'preserve\n' > "$runtime_root/envs/0.0.3.oh-my-setting-stage/user-file"
  OMS_TEST_UV_FAIL=1 oms_python_runtime_ensure "$runtime_uv" 0.0.3 >/dev/null 2>&1 || true
  [ -f "$runtime_root/envs/0.0.3.oh-my-setting-stage/user-file" ] || fail "runtime deleted an unowned stage"
  for child in envs managed launchers bin; do
    foreign_root="$TMP/foreign-runtime-$child"
    mkdir -p "$foreign_root" "$TMP/foreign-runtime-target"
    printf 'schema=1\nowner=oh-my-setting\n' > "$foreign_root/.oh-my-setting-python-runtime"
    ln -s "$TMP/foreign-runtime-target" "$foreign_root/$child"
    if OMS_PYTHON_RUNTIME_ROOT="$foreign_root" oms_python_runtime_ensure "$runtime_uv" "$runtime_version" >/dev/null 2>&1; then
      fail "runtime accepted a symlinked $child directory"
    fi
  done
)
grep -Fq -- "venv --managed-python --no-project --no-config --python $runtime_version" "$runtime_uv_log" ||
  fail "private runtime allowed project or system-Python fallback"
! grep '^env ' "$runtime_uv_log" | grep -Fvx 'env downloads=manual preference= managed= certs=1' > /dev/null ||
  fail "user uv settings leaked into the private runtime build: $(grep '^env ' "$runtime_uv_log" | sort -u)"

# The bootstrap reads the lock before any Python exists; it must agree with
# tool-lock.py on every key it can read, whatever the lock's formatting.
for key in $("$real_python3" - "$ROOT/tools.lock.json" <<'PY'
import json, sys
def walk(value, path):
    if isinstance(value, dict):
        for key, item in value.items():
            yield from walk(item, path + [key])
    elif not isinstance(value, list):
        yield ".".join(path)
for key in walk(json.load(open(sys.argv[1], encoding="utf-8")), []):
    print(key)
PY
); do
  [ "$(oms_lock_scalar "$ROOT/tools.lock.json" "$key")" = \
    "$("$real_python3" "$ROOT/scripts/lib/tool-lock.py" --lock "$ROOT/tools.lock.json" get "$key" | tr -d '\r')" ] ||
    fail "Python-free lock reader disagrees with tool-lock.py on $key"
done
# The two readers disagree on a repeated key (awk takes the first, json.loads
# the last), so validation must refuse a lock that repeats one.
"$real_python3" - "$ROOT/tools.lock.json" "$TMP/duplicate.lock.json" <<'PY'
import sys
text = open(sys.argv[1], encoding="utf-8").read()
open(sys.argv[2], "w", encoding="utf-8").write(text.replace('"python": {', '"python": {"version": "0.0.1", ', 1))
PY
if "$real_python3" "$ROOT/scripts/lib/tool-lock.py" --lock "$TMP/duplicate.lock.json" validate >/dev/null 2>&1; then
  fail "tool-lock validation accepted a lock that repeats a key"
fi

# Python decodes escaped JSON strings, but the bootstrap scalar reader cannot.
# A valid lock with one must fail closed instead of returning a wrong value.
escaped_lock="$TMP/escaped-tools.lock.json"
escaped_value_lock="$TMP/escaped-value-tools.lock.json"
"$real_python3" - "$ROOT/tools.lock.json" "$escaped_lock" "$escaped_value_lock" <<'PY'
import pathlib, re, sys
source = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
assert '"schema"' in source
pathlib.Path(sys.argv[2]).write_text(
    source.replace('"schema"', '"\\u0073chema"', 1), encoding="utf-8"
)
value_source, count = re.subn(
    r'("node"\s*:\s*\{\s*"version"\s*:\s*")([^"]+)',
    lambda match: match.group(1) + match.group(2).replace(".", "\\u002e", 1),
    source, count=1,
)
assert count == 1 and value_source != source
pathlib.Path(sys.argv[3]).write_text(value_source, encoding="utf-8")
PY
"$real_python3" "$ROOT/scripts/lib/tool-lock.py" --lock "$escaped_lock" validate >/dev/null ||
  fail "escaped lock fixture must remain valid JSON and contract"
"$real_python3" "$ROOT/scripts/lib/tool-lock.py" --lock "$escaped_value_lock" validate >/dev/null ||
  fail "escaped value fixture must remain valid JSON and contract"
if oms_lock_scalar "$escaped_lock" schema >/dev/null 2>&1; then
  fail "scalar reader accepted an escaped lock key"
fi
if oms_lock_scalar "$escaped_value_lock" node.version >/dev/null 2>&1; then
  fail "scalar reader accepted an escaped lock value"
fi
if OH_MY_SETTING_TOOL_LOCK="$escaped_lock" bash "$ROOT/scripts/doctor.sh" --tool-lock \
    >"$TMP/escaped-doctor.out" 2>&1; then
  fail "doctor accepted a lock the scalar reader cannot decode"
fi
grep -Fq 'fail: tool lock: unreadable schema' "$TMP/escaped-doctor.out" ||
  fail "doctor did not report the escaped lock read failure"

# A host with neither python3 nor uv: the pinned uv is fetched and verified,
# the runtime is built, and the managed shim names its stable launcher. No
# step may run python3.
boot="$TMP/bootstrap"
mkdir -p "$boot/archive/uv-fixture" "$boot/bin"
cp "$runtime_uv" "$boot/archive/uv-fixture/uv"
printf '#!/usr/bin/env bash\nexit 0\n' > "$boot/archive/uv-fixture/uvx"
chmod +x "$boot/archive/uv-fixture/uvx"
tar -czf "$boot/uv.tar.gz" -C "$boot/archive" uv-fixture
boot_platform="$(oms_tool_platform)"
"$real_python3" - "$runtime_repo/tools.lock.json" "$boot_platform" "$(oms_sha256_file "$boot/uv.tar.gz")" <<'PY'
import json, sys
path, platform, digest = sys.argv[1:]
row = json.load(open(path, encoding="utf-8"))
row["uv"]["platforms"][platform].update(url="https://example.invalid/uv.tar.gz", sha256=digest)
json.dump(row, open(path, "w", encoding="utf-8"), indent=2)
PY
printf '#!/usr/bin/env bash\nwhile [ "$#" -gt 1 ]; do [ "$1" = -o ] && cp "%s" "$2"; shift; done\n' \
  "$boot/uv.tar.gz" > "$boot/bin/curl"
printf '#!/usr/bin/env bash\necho called >> "%s"\nexit 97\n' "$boot/python-called" > "$boot/bin/python3"
chmod +x "$boot/bin/curl" "$boot/bin/python3"
(
  export PATH="$boot/bin:$PATH" OMS_PYTHON_RUNTIME_ROOT="$boot/runtime" \
    OMS_PYTHON_RUNTIME_UV_BIN_DIR="$boot/uv/bin"
  # shellcheck source=scripts/lib/file-lock.sh
  . "$ROOT/scripts/lib/file-lock.sh"
  # shellcheck source=scripts/lib/python-runtime.sh
  . "$ROOT/scripts/lib/python-runtime.sh"
  oms_python_runtime_bootstrap "$runtime_repo" > "$boot/out" 2>&1 ||
    fail "Python-free bootstrap failed: $(cat "$boot/out")"
  [ ! -e "$boot/python-called" ] || fail "the Python-free bootstrap ran python3"
  [ "$(sed -n 1p "$boot/uv/bin/uv.oh-my-setting-managed")" = "sha256=$(oms_sha256_file "$boot/uv/bin/uv")" ] ||
    fail "bootstrapped uv lacks the owner record doctor and ensure_uv read"
  printf '%s\n' '#!/usr/bin/env bash' '# managed by oh-my-setting' \
    "exec \"$OMS_PYTHON_RUNTIME_ROOT/bin/python3\" \"\$@\"" > "$boot/python3-shim"
  chmod +x "$boot/python3-shim"
  oms_install_python_shim_owned "$boot/python3-shim" || fail "runtime-backed shim was not recognized"
  [ "$("$boot/python3-shim" -c 'print("runtime-shim-ok")')" = runtime-shim-ok ] ||
    fail "runtime-backed shim did not run the OMS runtime"
  "$real_python3" - "$runtime_repo/tools.lock.json" "$boot_platform" <<'PY'
import json, sys
path, platform = sys.argv[1:]
row = json.load(open(path, encoding="utf-8"))
row["uv"]["platforms"][platform]["sha256"] = "0" * 64
json.dump(row, open(path, "w", encoding="utf-8"), indent=2)
PY
  rm -rf "$boot/uv" "$boot/runtime"
  if oms_python_runtime_bootstrap "$runtime_repo" > "$boot/out" 2>&1; then
    fail "a uv archive with the wrong sha256 was accepted"
  fi
  [ ! -e "$boot/uv/bin/uv" ] || fail "an unverified uv was installed"
)

# A scheduled bootstrap failure replaces stale success, before any Git mutation.
printf '%s\n' '#!/usr/bin/env bash' 'echo "error: fixture runtime unavailable" >&2' 'exit 9' \
  > "$runtime_repo/scripts/python-runtime.sh"
runtime_state="$TMP/runtime-auto.status"
printf 'status=up_to_date\n' > "$runtime_state"
if (
  # Only this copied updater runs as the fixture's own parent scheduler: a
  # worker's inherited child authority would refuse before the runtime stub.
  export HOME="$TMP/runtime-auto-home"
  mkdir -p "$HOME"
  export TMPDIR="$HOME"
  # shellcheck source=tests/lib/smoke-env.sh
  . "$ROOT/tests/lib/smoke-env.sh"
  OMS_AUTO_UPDATE_MANAGED=1 OMS_PYTHON_RUNTIME_ROOT="$TMP/missing-runtime" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$runtime_state" \
    OH_MY_SETTING_AUTO_UPDATE_LOG="$TMP/runtime-auto.log" \
    "$runtime_repo/scripts/auto-update.sh" apply
) > "$TMP/runtime-auto.out" 2>&1; then
  fail "scheduled update ignored runtime setup failure"
fi
grep -Fq 'status=failed' "$runtime_state" &&
  grep -Fq 'fixture runtime unavailable' "$runtime_state" ||
  fail "scheduled runtime failure left stale success visible"

# Windows CRLF in a value bash reads back — see install-contract.sh for why it
# breaks paths and state words. Simulated with a python3 that emits CRLF, so the
# regression is reachable on every platform, not only on a Windows runner.
crlf_bin="$TMP/crlf-bin"
real_python="$(copyable_python)"
mkdir -p "$crlf_bin"
{
  printf '%s\n' '#!/usr/bin/env bash'
  printf '%s\n' 'set -o pipefail'
  printf '%s\n' "\"$real_python\" \"\$@\" | sed 's/\$/\\r/'"
} > "$crlf_bin/python3"
chmod +x "$crlf_bin/python3"

[ "$(PATH="$crlf_bin:$PATH" python3 -c 'print("probe")' | od -c |
  grep -c '\\r')" -gt 0 ] ||
  fail "the CRLF python stub did not actually emit a carriage return"

crlf_source="$TMP/crlf-source.txt"
crlf_target="$TMP/crlf-target.txt"
printf 'v1\n' > "$crlf_source"
OMS_PLATFORM_OVERRIDE=windows oms_install_materialize "$crlf_source" "$crlf_target"

state="$(PATH="$crlf_bin:$PATH" oms_install_target_state "$crlf_source" "$crlf_target")"
[ "$state" = copy-current ] ||
  fail "a CRLF helper broke the managed state word: [$state]"
PATH="$crlf_bin:$PATH" oms_install_target_matches "$crlf_source" "$crlf_target" ||
  fail "a CRLF helper broke managed target recognition"

# A path read back through a helper has to stay usable as a path.
recovered="$(PATH="$crlf_bin:$PATH" oms_install_target_owned_source_under \
  "$TMP" "$crlf_target")" || fail "owned source lookup failed under CRLF"
[ -f "$recovered" ] ||
  fail "a CRLF helper returned a path that does not exist: [$recovered]"

[ "$(oms_strip_cr "$(printf 'value\r')")" = value ] ||
  fail "oms_strip_cr did not remove a carriage return"

# An update transaction stages install-contract.sh without platform.sh, which is
# why the contract carries a fallback for the platform helpers at all. Nothing
# checked that the fallback is complete, and it was not: adding a helper on the
# platform side made a staged contract call an undefined function and a signalled
# update exit 1 instead of 143. Source it alone and use it.
alone="$TMP/contract-alone"
mkdir -p "$alone"
cp "$ROOT/scripts/lib/install-contract.sh" "$alone/install-contract.sh"
cp "$ROOT/scripts/lib/managed-target.py" "$alone/managed-target.py"
bash -c "
set -euo pipefail
. '$alone/install-contract.sh'
[ \"\$(oms_strip_cr \"\$(printf 'v\r')\")\" = v ] || exit 1
[ \"\$(oms_install_link_mode)\" = symlink ] || exit 1
oms_install_python_shim_owned '$alone/install-contract.sh' && exit 1
[ \"\$(oms_install_target_state '$alone/install-contract.sh' '$alone/absent')\" = missing ] || exit 1
oms_install_receipt_field commit '$alone/no-receipt.json' && exit 1
exit 0
" || fail "install-contract.sh must work when staged without platform.sh"

# Ownership is decided by comparing the recorded source against the expected
# one, so the same directory spelled two ways is not the same source. macOS
# TMPDIR ends in a slash and BSD mktemp keeps the resulting "//" where GNU
# mktemp collapses it, which is why the first macOS run failed and why the
# condition cannot be reached through TMPDIR on Linux. Built explicitly here so
# the contract is pinned and locally reproducible on every platform.
spell_dir="$TMP/spelling"
mkdir -p "$spell_dir"
spell_source="$spell_dir/source.txt"
printf 'v1\n' > "$spell_source"

# The two modes do not agree on spelling, and that asymmetry is deliberate only
# in the sense that nothing depends on it: copy mode compares realpaths through
# managed-target.py and so accepts any spelling, while the symlink branch
# compares the readlink string exactly and does not. It stays invisible because
# every caller derives the source from $ROOT, which is pwd -P output. So the
# contract worth pinning is the one both modes share — a normalized source is
# recognized — plus the rule that produces it, since a caller that skips the
# normalization gets a spurious backup instead of its own link.
for mode in symlink copy; do
  spell_target="$spell_dir/target-$mode"
  OMS_PLATFORM_OVERRIDE=windows OH_MY_SETTING_LINK_MODE="$mode" \
    oms_install_materialize "$spell_source" "$spell_target"
  oms_install_target_matches "$spell_source" "$spell_target" ||
    fail "$mode: a pwd -P normalized source must be recognized"
done
[ "$(cd "$spell_dir" && pwd -P)/source.txt" = "$spell_source" ] ||
  fail "pwd -P must collapse the spelling every comparison relies on"
# The macOS failure in one line: the unnormalized spelling is a different string,
# and the symlink branch compares strings.
[ "$spell_dir//source.txt" != "$spell_source" ] ||
  fail "the double-slash spelling must actually differ for this to be a risk"

# Parameter-expansion parents keep the slashes dirname trims. A doubled slash
# before the receipt name left "link/", and -L follows a trailing slash, so the
# symlinked-directory refusal never fired. The hpc profile installs nothing,
# and every path is temporary, so the child guard is not what is under test.
receipt_root="$TMP/receipt"
mkdir -p "$receipt_root/real" "$receipt_root/user-home"
ln -s "$receipt_root/real" "$receipt_root/link"
receipt_apply() {
  OMS_HARNESS_CHILD=0 HOME="$receipt_root/user-home" \
    XDG_CONFIG_HOME="$receipt_root/user-home/.config" \
    OMS_INSTALL_LIFECYCLE_LOCK="$receipt_root/install-lifecycle.lock.d" \
    OMS_INSTALL_LIFECYCLE_LOCK_TIMEOUT=1 \
    "$ROOT/scripts/install-profile.sh" --apply --profile hpc \
    --primary-provider codex --allow-missing --receipt "$1"
}
receipt_rc=0
receipt_apply "$receipt_root/link//capabilities.json" \
  >"$receipt_root/link.out" 2>&1 || receipt_rc=$?
[ "$receipt_rc" = 2 ] &&
  grep -Fq 'receipt directory must not be a symbolic link' "$receipt_root/link.out" ||
  fail "a doubled slash bypassed the receipt symlink refusal: $(cat "$receipt_root/link.out")"
receipt_rc=0
receipt_apply "$receipt_root/link/sub/" >"$receipt_root/link.out" 2>&1 || receipt_rc=$?
[ "$receipt_rc" = 2 ] &&
  grep -Fq 'receipt directory must not be a symbolic link' "$receipt_root/link.out" ||
  fail "a trailing slash bypassed the receipt symlink refusal: $(cat "$receipt_root/link.out")"
[ -z "$(ls -A "$receipt_root/real")" ] ||
  fail "the receipt apply wrote through a symlinked directory"
receipt_apply "$receipt_root/real//capabilities.json" >/dev/null ||
  fail "a doubled slash under a real directory must still write the receipt"
[ -f "$receipt_root/real/capabilities.json" ] ||
  fail "the doubled-slash control wrote no receipt"

# Libraries sourced by bare name (BASH_SOURCE without a slash) resolve from the
# current directory, as dirname's "." did.
for lib in agent-install-state.sh install-contract.sh install-lifecycle-lock.sh file-lock.sh; do
  bare_dirs="$(cd "$ROOT/scripts/lib" && bash -c '
    unset ROOT
    shopt -u sourcepath
    . "$1" || exit 1
    printf "%s|%s|%s|%s\n" "${ROOT:-}" "${OMS_INSTALL_CONTRACT_LIB_DIR:-}" \
      "${OMS_INSTALL_LIFECYCLE_LIB_DIR:-}" "${OMS_FILE_LOCK_LIB_DIR:-}"' _ "$lib" 2>&1)" ||
    fail "bare-name source of $lib failed: $bare_dirs"
  case "$lib" in
    agent-install-state.sh) expected="$ROOT|||" ;;
    install-contract.sh) expected="|$ROOT/scripts/lib||" ;;
    install-lifecycle-lock.sh) expected="||$ROOT/scripts/lib|$ROOT/scripts/lib" ;;
    file-lock.sh) expected="|||$ROOT/scripts/lib" ;;
  esac
  [ "$bare_dirs" = "$expected" ] ||
    fail "bare-name source of $lib resolved '$bare_dirs', want '$expected'"
done

# Unlink restores the newest backup beside its target. A dotfiles-managed
# parent directory is a symlink, and find must still look inside it while
# returning the caller's spelling.
backup_root="$TMP/backup-parent"
mkdir -p "$backup_root/real"
: > "$backup_root/real/target.backup.20250101000000"
: > "$backup_root/real/target.backup.20250102000000"
ln -s "$backup_root/real" "$backup_root/link"
(
  # shellcheck source=scripts/lib/agent-install-state.sh
  . "$ROOT/scripts/lib/agent-install-state.sh"
  found="$(oms_ops_latest_backup "$backup_root/link/target")"
  [ "$found" = "$backup_root/link/target.backup.20250102000000" ] ||
    fail "latest backup through a symlinked parent was '$found'"
)

# Bash 5 floor. BASH_VERSINFO is read-only, so a "Bash 3.2" is the real script
# with its guard's version lookup falling back to OMS_FAKE_BASH_MAJOR, and a
# "Bash 5 on a Homebrew path" is a wrapper that logs its calls.
test_bash_floor() (
  local d="$TMP/bash-floor" real_bash="$BASH" name src out rc log
  local hint='Bash 5 or newer'
  mkdir -p "$d/x" "$d/brew/bin" "$d/old" "$d/home" "$d/cfg"
  fake_major() {  # SRC DST: version lookup consults OMS_FAKE_BASH_MAJOR first
    sed 's|\${BASH_VERSINFO\[0\]:-0}|${OMS_FAKE_BASH_MAJOR:-${BASH_VERSINFO[0]:-0}}|g' "$1" > "$2"
    grep -Fq OMS_FAKE_BASH_MAJOR "$2" || fail "no Bash version guard in $1"
  }
  log="$d/brew.log"
  printf '%s\n' '#!/bin/sh' "echo \"\$0 \$*\" >> '$log'" \
    "exec env -u OMS_FAKE_BASH_MAJOR '$real_bash' \"\$@\"" > "$d/brew/bin/bash"
  printf '%s\n' '#!/bin/sh' 'exit 1' > "$d/old/bash"
  chmod +x "$d/brew/bin/bash" "$d/old/bash"
  export HOME="$d/home" OMS_INSTALL_RECEIPT="$d/cfg/install.json" \
    OMS_PYTHON_RUNTIME_ROOT="$d/missing-runtime"
  unset OMS_BASH OMS_INSTALL_BASH_REEXEC

  # Entry points: the dispatcher, every hook script, and the scheduled ones.
  for name in oms skill-router.sh turn-guard.sh fail-ledger-hook.sh syntax-guard-hook.sh \
      git-move-guard-hook.sh precompact-handoff.sh \
      resume-hook.sh telemetry-hook.sh auto-update.sh tick.sh; do
    fake_major "$ROOT/scripts/$name" "$d/x/$name"
    rc=0
    out="$(OMS_FAKE_BASH_MAJOR=3 bash "$d/x/$name" list </dev/null 2>&1)" || rc=$?
    want=1
    [ "$name" != git-move-guard-hook.sh ] || want=2
    [ "$rc" -eq "$want" ] || fail "$name under Bash 3.2 exited $rc, want $want: $out"
    case "$out" in "error: oh-my-setting needs $hint"*'brew install bash, then rerun') ;;
      *) fail "$name under Bash 3.2 gave: $out" ;; esac
  done
  out="$(OMS_FAKE_BASH_MAJOR=5 bash "$d/x/oms" list 2>&1 || true)"
  case "$out" in *"$hint"*) fail "dispatcher refused Bash 5: $out" ;; esac

  # Hook launcher: nothing recorded fails closed, an unusable record too.
  mkdir -p "$d/root/scripts/lib"
  fake_major "$ROOT/scripts/lib/python-entry.sh" "$d/root/scripts/lib/python-entry.sh"
  cp "$ROOT/scripts/lib/python-runtime.sh" "$ROOT/scripts/lib/platform.sh" "$d/root/scripts/lib/"
  printf '%s\n' 'echo hook-ran' > "$d/root/scripts/dummy-hook.sh"
  cp "$d/root/scripts/dummy-hook.sh" "$d/root/scripts/git-move-guard-hook.sh"
  rc=0
  out="$(OMS_FAKE_BASH_MAJOR=3 bash "$d/root/scripts/lib/python-entry.sh" bash "$d/root/scripts/dummy-hook.sh" 2>&1)" || rc=$?
  [ "$rc" -eq 1 ] && case "$out" in "error: oh-my-setting needs $hint"*) true ;; *) false ;; esac ||
    fail "hook launcher without a record: $rc $out"
  rc=0
  OMS_FAKE_BASH_MAJOR=3 bash "$d/root/scripts/lib/python-entry.sh" bash "$d/root/scripts/git-move-guard-hook.sh" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 2 ] || fail "git-move guard through the launcher exited $rc, want 2"
  printf '%s\n' "$d/old/bash" > "$d/cfg/bash-interpreter"
  rc=0
  OMS_FAKE_BASH_MAJOR=3 bash "$d/root/scripts/lib/python-entry.sh" bash "$d/root/scripts/dummy-hook.sh" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 1 ] || fail "launcher accepted a record that is not Bash 5 (exit $rc)"

  # The record: written by install, replaced atomically, kept when nothing usable.
  rm -f "$d/cfg/bash-interpreter"
  (
    . "$ROOT/scripts/lib/install-contract.sh"
    OMS_BASH="$d/old/bash" BASH="$d/old/bash" oms_install_bash_write_record
    [ ! -e "$d/cfg/bash-interpreter" ] || fail "recorded a Bash that is not 5"
    OMS_BASH="$d/brew/bin/bash" oms_install_bash_write_record
    [ "$(cat "$d/cfg/bash-interpreter")" = "$d/brew/bin/bash" ] || fail "record was not written"
    OMS_BASH="$d/old/bash" BASH="$d/old/bash" oms_install_bash_write_record
    [ "$(cat "$d/cfg/bash-interpreter")" = "$d/brew/bin/bash" ] || fail "record lost when nothing usable"
    [ "$(find "$d/cfg" -name 'bash-interpreter.*' | wc -l)" -eq 0 ] || fail "record write left a stage file"
  )

  # A registered hook command, started by Bash 3.2, runs under the record.
  hook_command="$(python3 - "$ROOT" "$d/root" "$d/root/scripts/dummy-hook.sh" <<'PY_ENTRY'
import importlib.util, sys
from pathlib import Path
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("hooks", Path(sys.argv[1]) / "scripts/lib/claude-hook-surfaces.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(module.entry_command(sys.argv[2], "bash", sys.argv[3]))
PY_ENTRY
)"
  : > "$log"
  out="$(OMS_FAKE_BASH_MAJOR=3 bash -c "$hook_command" 2>&1)" || fail "hook under Bash 3.2 failed: $out"
  [ "$out" = 'hook-ran' ] || fail "hook output was: $out"
  grep -Fq "$d/brew/bin/bash $d/root/scripts/lib/python-entry.sh bash $d/root/scripts/dummy-hook.sh" "$log" &&
    grep -Fq "$d/brew/bin/bash $d/root/scripts/dummy-hook.sh" "$log" ||
    fail "hook command did not run under the recorded Bash: $(cat "$log")"
  rm -f "$d/cfg/bash-interpreter"

  # Children of a Bash 5 launch must not fall back to the system Bash 3.2 that
  # a cron/hook PATH puts first. Here the fake version is a `major` file beside
  # the interpreter that runs the script (copies, so they differ by inode): the stub on PATH is "3.2", the
  # recorded one (off PATH) is real.
  mkdir -p "$d/stub32" "$d/brew2/bin" "$d/root2/scripts/lib"
  cp "$real_bash" "$d/stub32/bash"
  printf '3\n' > "$d/stub32/major"
  cp "$real_bash" "$d/brew2/bin/bash"
  fake_dir_major() {  # SRC DST
    sed 's~\${BASH_VERSINFO\[0\]:-0}~$(cat "${BASH%/*}/major" 2>/dev/null || echo "${BASH_VERSINFO[0]:-0}")~g' "$1" > "$2"
    grep -Fq '/major' "$2" || fail "no Bash version guard in $1"
  }
  fake_dir_major "$ROOT/scripts/oms" "$d/oms.dirfake"
  { sed -n 1,2p "$d/oms.dirfake"; echo 'echo child-ran'; } > "$d/root2/scripts/child.sh"
  printf '%s\n' '#!/usr/bin/env bash' 'exec "${0%/*}/child.sh"' > "$d/root2/scripts/hook.sh"
  chmod +x "$d/root2/scripts/child.sh" "$d/root2/scripts/hook.sh"
  fake_dir_major "$ROOT/scripts/lib/python-entry.sh" "$d/root2/scripts/lib/python-entry.sh"
  cp "$ROOT/scripts/lib/python-runtime.sh" "$ROOT/scripts/lib/platform.sh" "$d/root2/scripts/lib/"
  rc=0
  PATH="$d/stub32:$PATH" "$d/root2/scripts/child.sh" >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 1 ] || fail "control: the floor-guarded child ran under the stub Bash 3.2 (exit $rc)"
  printf '%s\n' "$d/brew2/bin/bash" > "$d/cfg/bash-interpreter"
  out="$(PATH="$d/stub32:$PATH" "$d/stub32/bash" "$d/root2/scripts/lib/python-entry.sh" bash "$d/root2/scripts/hook.sh" 2>&1)" ||
    fail "a hook's floor-guarded child failed under the recorded Bash: $out"
  [ "$out" = child-ran ] || fail "hook child output was: $out"

  # tick.sh started under the recorded Bash: its oms calls and its timer PATH.
  farm="$d/farm"
  mkdir -p "$farm/scripts/lib"
  for src in "$ROOT"/*; do
    [ "${src##*/}" = scripts ] || ln -s "$src" "$farm/${src##*/}"
  done
  for src in "$ROOT"/scripts/* "$ROOT"/scripts/lib/*; do
    case "$src" in */scripts/oms|*/scripts/tick.sh|*/scripts/auto-update.sh|*/scripts/lib) continue ;; esac
    ln -s "$src" "${farm}/${src#"$ROOT"/}"
  done
  fake_dir_major "$ROOT/scripts/oms" "$farm/scripts/oms"
  sed -i '2a echo "$*" >> "$OMS_TEST_OMS_LOG"' "$farm/scripts/oms"
  fake_dir_major "$ROOT/scripts/tick.sh" "$farm/scripts/tick.sh"
  chmod +x "$farm/scripts/oms" "$farm/scripts/tick.sh"
  mkdir -p "$d/tick-repo/.oms" "$d/state" "$d/rt"
  git -C "$d/tick-repo" init -q
  printf '*\n' > "$d/tick-repo/.oms/.gitignore"
  export OMS_TEST_OMS_LOG="$d/tick-oms.log"
  : > "$OMS_TEST_OMS_LOG"
  XDG_CONFIG_HOME="$d/xdg" XDG_RUNTIME_DIR="$d/rt" XDG_STATE_HOME="$d/state" \
    OMS_WORK_JOURNAL_CONFIG="$d/no-journal.json" OMS_HARNESS_CHILD=0 \
    PATH="$d/stub32:$PATH" "$d/brew2/bin/bash" "$farm/scripts/tick.sh" run --repo "$d/tick-repo" \
    >"$d/tick.out" 2>&1 || fail "tick under the recorded Bash failed: $(cat "$d/tick.out")"
  grep -Fq 'agent-events' "$OMS_TEST_OMS_LOG" ||
    fail "tick's oms calls hit the floor guard under the stub Bash: $(cat "$d/tick.out")"
  out="$(XDG_CONFIG_HOME="$d/xdg" "$d/brew2/bin/bash" "$farm/scripts/tick.sh" install --method cron --dry-run 2>&1)"
  case "$out" in *":$d/brew2/bin:"*|*"=$d/brew2/bin:"*) ;; *) fail "tick timer PATH lacks the recorded Bash: $out" ;; esac

  # auto-update.sh enters Bash 5 before it activates Python and runs update.sh.
  rm "$farm/scripts/lib/python-runtime.sh"
  printf '%s\n' 'oms_python_runtime_activate_if_present() { echo "OMS_BASH=${OMS_BASH:-unset} bash=$(command -v bash)" > "$OMS_TEST_OMS_LOG"; }' \
    > "$farm/scripts/lib/python-runtime.sh"
  fake_dir_major "$ROOT/scripts/auto-update.sh" "$farm/scripts/auto-update.sh"
  chmod +x "$farm/scripts/auto-update.sh"
  PATH="$d/stub32:$PATH" "$d/brew2/bin/bash" "$farm/scripts/auto-update.sh" status >/dev/null 2>&1 || true
  [ "$(cat "$OMS_TEST_OMS_LOG")" = "OMS_BASH=$d/brew2/bin/bash bash=$d/brew2/bin/bash" ] ||
    fail "auto-update.sh did not enter Bash 5 first: $(cat "$OMS_TEST_OMS_LOG")"

  # The interpreter lookup falls back to the Homebrew locations (what a repair
  # run under Bash 3.2 has), and to nothing when none is usable.
  rm -f "$d/cfg/bash-interpreter"
  (
    . "$ROOT/scripts/lib/install-contract.sh"
    unset OMS_BASH
    BASH="$d/old/bash"
    out="$(OMS_BASH_CANDIDATES="$d/none/bash $d/brew2/bin/bash" oms_install_bash_interpreter)" ||
      fail "no fallback to the Homebrew candidates"
    [ "$out" = "$d/brew2/bin/bash" ] || fail "fallback chose $out"
    ! OMS_BASH_CANDIDATES="" oms_install_bash_interpreter >/dev/null ||
      fail "an empty candidate list still found an interpreter"
    ! OMS_BASH_CANDIDATES="$d/old/bash" oms_install_bash_interpreter >/dev/null ||
      fail "a Bash that is not 5 was accepted as a fallback"
  )
  # Call sites that cannot run to completion here: install.sh enters after its
  # checkout is final, and repair refreshes the trigger that names the record.
  grep -A1 'lib/platform.sh"$' "$ROOT/install.sh" | grep -Fq oms_bash_enter ||
    fail "install.sh does not enter Bash 5 for its child scripts"
  grep -Fq 'install-autoupdate.sh' "$ROOT/scripts/doctor.sh" ||
    fail "doctor --repair does not refresh the auto-update trigger"
  rm -f "$d/cfg/bash-interpreter"

  # install.sh: re-exec under a found Bash 5, or stop with the hint.
  fake_major "$ROOT/install.sh" "$d/install.sh"
  : > "$log"
  out="$(OMS_FAKE_BASH_MAJOR=3 OMS_BASH_CANDIDATES="$d/none/bash $d/old/bash $d/brew/bin/bash" \
    bash "$d/install.sh" --help 2>&1)" || fail "install.sh did not re-exec under Bash 5: $out"
  case "$out" in Usage:*) ;; *) fail "re-executed install.sh printed: $out" ;; esac
  grep -Fq "$d/brew/bin/bash $d/install.sh --help" "$log" || fail "install.sh did not use the Homebrew Bash"
  for candidates in "$d/none/bash $d/old/bash" ""; do
    : > "$log"
    rc=0
    out="$(OMS_FAKE_BASH_MAJOR=3 OMS_BASH_CANDIDATES="$candidates" bash "$d/install.sh" --help 2>&1)" || rc=$?
    [ "$rc" -eq 1 ] && case "$out" in *'brew install bash'*) true ;; *) false ;; esac ||
      fail "install.sh without a Bash 5 exited $rc: $out"
    [ ! -s "$log" ] || fail "install.sh ran a Bash that is not 5"
  done
  rc=0
  out="$(OMS_INSTALL_BASH_REEXEC=1 OMS_FAKE_BASH_MAJOR=3 OMS_BASH_CANDIDATES="$d/brew/bin/bash" \
    bash "$d/install.sh" --help 2>&1)" || rc=$?
  [ "$rc" -eq 1 ] && [ ! -s "$log" ] || fail "install.sh re-executed twice: $rc $out"
  rc=0
  out="$(OMS_FAKE_BASH_MAJOR=3 OMS_BASH_CANDIDATES="$d/brew/bin/bash" bash -s -- --help < "$d/install.sh" 2>&1)" || rc=$?
  [ "$rc" -eq 1 ] && case "$out" in *"rerun the installer with $d/brew/bin/bash"*) true ;; *) false ;; esac ||
    fail "piped install.sh gave: $rc $out"

  # Uninstall removes the record with the receipt.
  mkdir -p "$d/home/.local/bin"
  (
    . "$ROOT/scripts/lib/install-contract.sh"
    oms_install_write_receipt "$ROOT" >/dev/null
    OMS_BASH="$d/brew/bin/bash" oms_install_bash_write_record
  )
  [ -f "$d/cfg/bash-interpreter" ] || fail "fixture record missing"
  bash "$ROOT/scripts/unlink.sh" --dry-run >/dev/null 2>&1 || true
  [ -f "$d/cfg/bash-interpreter" ] || fail "dry-run removed the record"
  bash "$ROOT/scripts/unlink.sh" >/dev/null 2>&1 || fail "unlink failed"
  [ ! -e "$d/cfg/bash-interpreter" ] && [ ! -e "$d/cfg/install.json" ] ||
    fail "unlink left the interpreter record or receipt"
  echo 'bash-floor: ok'
)
test_bash_floor

echo "platform-portability: ok"
