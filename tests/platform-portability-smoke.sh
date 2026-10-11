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

assert_regular_copy() {
  local source="$1"
  local target="$2"

  [ -e "$target" ] || fail "copy is missing: $target"
  [ ! -L "$target" ] || fail "copy unexpectedly became a symlink: $target"
  diff -qr "$source" "$target" >/dev/null 2>&1 ||
    fail "copy differs from source: $target"
}

# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"
# shellcheck source=scripts/lib/install-contract.sh
. "$ROOT/scripts/lib/install-contract.sh"

[ "$(OMS_PLATFORM_OVERRIDE=windows oms_platform_name)" = windows ] ||
  fail "Windows platform override was not detected"
[ "$(OMS_PLATFORM_OVERRIDE=windows oms_install_link_mode)" = copy ] ||
  fail "Windows must default to copy mode"
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

# The timer's private runtime is version-scoped and never replaces user data.
runtime_repo="$TMP/runtime-repo"
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
  --version) echo 'uv 0.12.3' ;;
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
  [ -z "${PYTHONHOME:-}${PYTHONPATH:-}" ] || fail "foreign Python configuration leaked"
  [ "$(python3 -c 'import sys; print("%d.%d.%d" % sys.version_info[:3])' | tr -d '\r')" = "$runtime_version" ] ||
    fail "private runtime selected a different interpreter"
  case "$(command -v python3)" in "$runtime_root/launchers/$runtime_version/python3") ;; *)
    fail "private runtime did not shadow system Python" ;; esac
  mkdir -p "$TMP/runtime-module"
  printf 'value = 42\n' > "$TMP/runtime-module/oms_runtime_probe.py"
  PYTHONPATH="$TMP/runtime-module" python3 -c 'import oms_runtime_probe; assert oms_runtime_probe.value == 42' ||
    fail "runtime launcher stripped an explicit OMS module path"

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
! grep '^env ' "$runtime_uv_log" | grep -Fvxq 'env downloads=manual preference= managed= certs=1' ||
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
if OMS_AUTO_UPDATE_MANAGED=1 OMS_PYTHON_RUNTIME_ROOT="$TMP/missing-runtime" \
    OH_MY_SETTING_AUTO_UPDATE_STATE="$runtime_state" \
    OH_MY_SETTING_AUTO_UPDATE_LOG="$TMP/runtime-auto.log" \
    "$runtime_repo/scripts/auto-update.sh" apply > "$TMP/runtime-auto.out" 2>&1; then
  fail "scheduled update ignored runtime setup failure"
fi
grep -Fq 'status=failed' "$runtime_state" &&
  grep -Fq 'fixture runtime unavailable' "$runtime_state" ||
  fail "scheduled runtime failure left stale success visible"

# Windows CRLF in a value bash reads back — see install-contract.sh for why it
# breaks paths and state words. Simulated with a python3 that emits CRLF, so the
# regression is reachable on every platform, not only on a Windows runner.
crlf_bin="$TMP/crlf-bin"
real_python="$(command -v python3)"
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

echo "platform-portability: ok"
