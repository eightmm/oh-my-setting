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

echo "platform-portability: ok"
