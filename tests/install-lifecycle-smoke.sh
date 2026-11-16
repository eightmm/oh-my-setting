#!/usr/bin/env bash
set -euo pipefail

# Pure-Rust install -> doctor -> tamper repair -> update -> uninstall E2E contract.
# Runs with interpreter lookup poisoned (`exit 99`) and zero network tool downloads.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-install-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TMP="$(cd "$TMP" && pwd -P)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

build_dir="$(oms_smoke_native_build_dir "$ROOT")" ||
  fail "pre-built native artifacts missing in $ROOT/rust/target/{release,debug}"

for mode in symlink copy; do
  case_dir="$TMP/mode-$mode"
  mkdir -p "$case_dir/remote" "$case_dir/checkout" "$case_dir/home/.local/bin" "$case_dir/bin" "$case_dir/locks"
  for tool in node uv gh ntn codex; do
    cat > "$case_dir/bin/$tool" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in --version|-V) echo "v99.0.0" ;; esac
exit 0
EOF
    chmod +x "$case_dir/bin/$tool"
  done

  git -C "$case_dir/checkout" init -qb main
  git -C "$case_dir/checkout" config user.name oms-test
  git -C "$case_dir/checkout" config user.email test@example.com
  tar -C "$ROOT" --exclude=.git --exclude=rust/target --exclude=.oms -cf - . | tar -C "$case_dir/checkout" -xf -
  git -C "$case_dir/checkout" add -A
  git -C "$case_dir/checkout" commit -qm "initial v2"
  rev1="$(git -C "$case_dir/checkout" rev-parse HEAD)"

  git -C "$case_dir/remote" init -q --bare
  git -C "$case_dir/checkout" remote add origin "$case_dir/remote"
  git -C "$case_dir/checkout" push -q origin main

  # Stage native binaries directly for rev1 (avoid slow recompilation inside test).
  oms_smoke_stage_native "$case_dir/checkout/rust/target/oms-native/$rev1" "$build_dir" ||
    fail "failed staging native binaries for $rev1 ($mode)"

  export HOME="$case_dir/home"
  export XDG_CONFIG_HOME="$HOME/.config"
  export XDG_DATA_HOME="$HOME/.local/share"
  export XDG_STATE_HOME="$HOME/.local/state"
  export XDG_CACHE_HOME="$HOME/.cache"
  export OMS_LOCK_DIR="$case_dir/locks"
  export OMS_INSTALL_LIFECYCLE_LOCK="$case_dir/locks/install-lifecycle.lock.d"
  export OMS_NATIVE_NO_AUTO_STAGE=1
  export OH_MY_SETTING_DIR="$case_dir/checkout"
  export OH_MY_SETTING_LINK_MODE="$mode"
  export OH_MY_SETTING_REF=main
  export OH_MY_SETTING_CLAUDE_HOOKS=0
  export OH_MY_SETTING_CODEX_PLUGIN=0
  export OH_MY_SETTING_AGY_PLUGIN=0
  export OH_MY_SETTING_MCP=0
  export OH_MY_SETTING_AUTO_UPDATE=0
  export OH_MY_SETTING_GENERATE_MACHINE=0
  export OH_MY_SETTING_GENERATE_SLURM=0
  export OH_MY_SETTING_CONNECT_SERVICES=0
  export OH_MY_SETTING_PRIMARY_PROVIDER=codex
  export PATH="$OMS_SMOKE_POISON_BIN:$case_dir/bin:$HOME/.local/bin:/usr/bin:/bin"

  out="$("$case_dir/checkout/install.sh" --no-auto-update 2>&1)" ||
    fail "install ($mode) failed: $out"
  [ -x "$HOME/.local/bin/oms" ] || fail "$HOME/.local/bin/oms missing ($mode)"
  if [ "$mode" = copy ]; then
    [ ! -L "$HOME/.local/bin/oms" ] || fail "copy mode created a symlinked oms"
  else
    [ -L "$HOME/.local/bin/oms" ] || fail "symlink mode did not symlink oms"
  fi
  jq -e --arg rev "$rev1" --arg mode "$mode" '.schema == 2 and .commit == $rev and .link_mode == $mode' \
    "$XDG_CONFIG_HOME/oh-my-setting/install.json" >/dev/null ||
    fail "receipt validation failed ($mode): $(cat "$XDG_CONFIG_HOME/oh-my-setting/install.json")"

  (
    cd "$case_dir/checkout"
    "$HOME/.local/bin/oms" list >/dev/null || fail "installed oms list failed ($mode)"
    "$HOME/.local/bin/oms" doctor --no-model-doctor >/dev/null ||
      fail "installed oms doctor failed ($mode)"

    # Corrupt staged executable -> strict command fails; doctor --repair quarantines & recovers.
    printf 'corrupt\n' > "$case_dir/checkout/rust/target/oms-native/$rev1/oms"
    chmod +x "$case_dir/checkout/rust/target/oms-native/$rev1/oms"
    corrupt_rc=0
    "$HOME/.local/bin/oms" list >/dev/null 2>&1 || corrupt_rc=$?
    [ "$corrupt_rc" -eq 2 ] || fail "corrupted oms list did not fail with exit 2 (got $corrupt_rc)"

    # Put a fast cargo shim on PATH only during repair so cargo builds instantly by copying build_dir.
    cat > "$case_dir/bin/cargo" <<EOF
#!/usr/bin/env bash
mkdir -p "$case_dir/checkout/rust/target/lifecycle-build/release"
cp -f "$build_dir"/oms* "$case_dir/checkout/rust/target/lifecycle-build/release/"
EOF
    chmod +x "$case_dir/bin/cargo"
    OMS_NATIVE_NO_AUTO_STAGE=0 "$HOME/.local/bin/oms" doctor --repair --no-model-doctor >/dev/null ||
      fail "doctor --repair failed ($mode)"
    "$HOME/.local/bin/oms" list >/dev/null || fail "oms list failed after repair ($mode)"
    broken_found=0
    for broken_dir in "$case_dir/checkout/rust/target/oms-native/.broken-$rev1"-*; do
      [ -d "$broken_dir" ] && broken_found=1 && break
    done
    [ "$broken_found" -eq 1 ] ||
      fail "broken native cache entry was not quarantined ($mode)"
    rm -f "$case_dir/bin/cargo"

    # Update check against clean remote, then clean uninstall.
    "$HOME/.local/bin/oms" update --check >/dev/null || fail "oms update --check failed ($mode)"
    "$HOME/.local/bin/oms" uninstall --yes >/dev/null || fail "oms uninstall failed ($mode)"
    [ ! -e "$HOME/.local/bin/oms" ] || fail "uninstall left ~/.local/bin/oms behind ($mode)"
  )
done

echo "install-lifecycle-smoke: ok"
