#!/usr/bin/env bash
set -euo pipefail

# Pure-Rust install -> doctor -> tamper repair -> update -> uninstall E2E contract.
# Runs with interpreter lookup poisoned (`exit 99`) and zero network tool downloads.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"

PLAN_ONLY=0
case "${1:-}" in
  "") ;;
  --plan-only) PLAN_ONLY=1 ;;
  *) echo "usage: install-lifecycle-smoke.sh [--plan-only]" >&2; exit 2 ;;
esac

TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-install-lifecycle.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
TMP="$(cd "$TMP" && pwd -P)"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

check_install_plan_without_staging() (
  plan_dir="$TMP/plan"
  checkout="$plan_dir/checkout"
  mkdir -p "$checkout/scripts/lib" "$checkout/crates/oms" "$plan_dir/bin" "$plan_dir/home"
  cp "$ROOT/install.sh" "$checkout/install.sh"
  cp "$ROOT/scripts/lib/platform.sh" "$checkout/scripts/lib/platform.sh"
  cp "$ROOT/Cargo.toml" "$ROOT/rust-toolchain.toml" "$checkout/"
  cp "$ROOT/crates/oms/Cargo.toml" "$checkout/crates/oms/Cargo.toml"
  oms_smoke_make_repo "$checkout"
  git -C "$checkout" add .
  git -C "$checkout" commit -qm fixture
  revision="$(git -C "$checkout" rev-parse HEAD)"
  ln -s "$BASH" "$plan_dir/bin/bash"
  for blocked in cargo rustup curl wget codex claude agy; do
    cat > "$plan_dir/bin/$blocked" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "${0##*/}" >> "$OMS_PLAN_FORBIDDEN_LOG"
exit 99
EOF
    chmod +x "$plan_dir/bin/$blocked"
  done
  run_plan() {
    env -i HOME="$plan_dir/home" PATH="$plan_dir/bin:/usr/bin:/bin" \
      GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1 OH_MY_SETTING_DIR="$checkout" \
      OMS_LOCK_DIR="$plan_dir/locks" OMS_NATIVE_NO_AUTO_STAGE=0 \
      OMS_PLAN_FORBIDDEN_LOG="$plan_dir/forbidden.log" \
      bash "$checkout/install.sh" --plan
  }
  status=0
  out="$(run_plan 2>&1)" || status=$?
  [ "$status" -eq 2 ] || fail "cold install plan must require explicit payload preparation: $out"
  grep -Fq -- '--plan does not build or download' <<< "$out" || fail "cold plan omitted its preparation prerequisite"
  [ ! -s "$plan_dir/forbidden.log" ] || fail "cold plan invoked Cargo, a download, or a provider"
  [ ! -e "$checkout/target" ] || fail "cold plan created a native staging/build directory"
  [ ! -e "$plan_dir/locks" ] || fail "cold plan acquired a lifecycle lock"
  [ -z "$(find "$plan_dir/home" -mindepth 1 -print)" ] || fail "cold plan wrote user state"

  # A caller can still preview an already prepared, checksum-verified payload.
  mkdir -p "$plan_dir/built"
  cat > "$plan_dir/built/oms" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${OMS_NATIVE_NO_AUTO_STAGE:-0}" = 1 ] || exit 98
[ "${4:-}" = -- ] && [ "${5:-}" = install ] && [ "${6:-}" = --plan ] || exit 98
case "${1:-}" in
  --oms-select) exit 0 ;;
  --oms-dispatch) printf 'fixture install plan\n' ;;
  *) exit 98 ;;
esac
EOF
  chmod +x "$plan_dir/built/oms"
  oms_smoke_stage_native "$checkout/target/oms-native/$revision" "$plan_dir/built"
  out="$(run_plan 2>&1)" || fail "verified warm install plan failed: $out"
  [ "$out" = 'fixture install plan' ] || fail "warm plan did not dispatch the verified payload: $out"
  [ ! -s "$plan_dir/forbidden.log" ] || fail "warm plan invoked Cargo, a download, or a provider"
  [ ! -e "$checkout/target/lifecycle-build" ] || fail "warm plan rebuilt the payload"
  [ ! -e "$plan_dir/locks" ] || fail "warm plan acquired a lifecycle lock"
  [ -z "$(find "$plan_dir/home" -mindepth 1 -print)" ] || fail "warm plan wrote user state"
)

check_install_plan_without_staging
if [ "$PLAN_ONLY" = 1 ]; then
  echo "install-lifecycle-smoke (--plan-only): ok"
  exit 0
fi

build_dir="$(oms_smoke_native_build_dir "$ROOT")" ||
  fail "pre-built native artifacts missing in $ROOT/target/{release,debug}"

for mode in symlink copy; do
  case_dir="$TMP/mode-$mode"
  mkdir -p "$case_dir/remote" "$case_dir/checkout" "$case_dir/home/.local/bin" "$case_dir/bin" "$case_dir/locks"
  ln -sf "$BASH" "$case_dir/bin/bash"
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
  tar -C "$ROOT" --exclude=.git --exclude=target --exclude=.oms -cf - . | tar -C "$case_dir/checkout" -xf -
  git -C "$case_dir/checkout" add -A
  git -C "$case_dir/checkout" commit -qm "initial v2"
  rev1="$(git -C "$case_dir/checkout" rev-parse HEAD)"

  git -C "$case_dir/remote" init -q --bare
  git -C "$case_dir/remote" symbolic-ref HEAD refs/heads/main
  git -C "$case_dir/checkout" remote add origin "$case_dir/remote"
  git -C "$case_dir/checkout" push -q origin main

  # Stage native binaries directly for rev1 (avoid slow recompilation inside test).
  oms_smoke_stage_native "$case_dir/checkout/target/oms-native/$rev1" "$build_dir" ||
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
  unset OH_MY_SETTING_REF OMS_INSTALL_RECEIPT
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
  receipt="$XDG_CONFIG_HOME/oh-my-setting/install.json"
  jq -e --arg rev "$rev1" --arg mode "$mode" \
    '.schema == 2 and .commit == $rev and .link_mode == $mode and .ref == "edge"' "$receipt" >/dev/null ||
    fail "first install should record edge ($mode): $(cat "$receipt")"

  if [ "$mode" = copy ]; then
    git -C "$case_dir/checkout" commit --allow-empty -qm "advance default branch"
    git -C "$case_dir/checkout" push -q origin main
    out="$(OH_MY_SETTING_REF=edge "$case_dir/checkout/install.sh" --ref "$rev1" --no-auto-update 2>&1)" ||
      fail "explicit ref pin failed: $out"
    jq -e --arg rev "$rev1" '.commit == $rev and .ref == $rev' "$receipt" >/dev/null ||
      fail "--ref did not override the environment"
    out="$("$case_dir/checkout/install.sh" --plan 2>&1)" ||
      fail "pinned install plan failed: $out"
    jq -e --arg rev "$rev1" '.install.ref == $rev' <<< "$out" >/dev/null ||
      fail "install plan changed the saved pin: $out"
    out="$("$case_dir/checkout/install.sh" --no-auto-update 2>&1)" ||
      fail "reinstall of the saved pin failed: $out"
    [ "$(git -C "$case_dir/checkout" rev-parse HEAD)" = "$rev1" ] ||
      fail "reinstall moved off the pinned checkout"
    jq -e --arg rev "$rev1" '.commit == $rev and .ref == $rev' "$receipt" >/dev/null ||
      fail "reinstall changed the saved pin"
    out="$(OH_MY_SETTING_REF=main "$case_dir/checkout/install.sh" --plan 2>&1)" ||
      fail "explicit environment install plan failed: $out"
    jq -e '.install.ref == "main"' <<< "$out" >/dev/null ||
      fail "install plan did not honor explicit OH_MY_SETTING_REF: $out"
  fi

  (
    cd "$case_dir/checkout"
    "$HOME/.local/bin/oms" list >/dev/null || fail "installed oms list failed ($mode)"
    "$HOME/.local/bin/oms" doctor --no-model-doctor >/dev/null ||
      fail "installed oms doctor failed ($mode)"

    # Corrupt staged executable -> strict command fails; doctor --repair quarantines & recovers.
    printf 'corrupt\n' > "$case_dir/checkout/target/oms-native/$rev1/oms"
    chmod +x "$case_dir/checkout/target/oms-native/$rev1/oms"
    corrupt_rc=0
    "$HOME/.local/bin/oms" list >/dev/null 2>&1 || corrupt_rc=$?
    [ "$corrupt_rc" -eq 2 ] || fail "corrupted oms list did not fail with exit 2 (got $corrupt_rc)"

    # Put a fast cargo shim on PATH only during repair so cargo builds instantly by copying build_dir.
    cat > "$case_dir/bin/cargo" <<EOF
#!/usr/bin/env bash
set -euo pipefail
[ "\$PWD" = "$case_dir/checkout" ] || exit 81
[ "\${1:-}" = build ] || exit 82
[ -f rust-toolchain.toml ] || exit 83
[ "\${CARGO_NET_OFFLINE:-}" = true ] || exit 84
[ "\${RUSTUP_TOOLCHAIN+x}" != x ] || exit 87
locked=0
for arg in "\$@"; do
  case "\$arg" in
    --offline|+*) exit 85 ;;
    --locked) locked=1 ;;
  esac
done
[ "\$locked" = 1 ] || exit 86
mkdir -p "$case_dir/checkout/target/lifecycle-build/release"
cp -f "$build_dir"/oms* "$case_dir/checkout/target/lifecycle-build/release/"
EOF
    chmod +x "$case_dir/bin/cargo"
    export RUSTUP_TOOLCHAIN=caller-toolchain
    CARGO_NET_OFFLINE=true OMS_NATIVE_NO_AUTO_STAGE=0 "$HOME/.local/bin/oms" doctor --repair --no-model-doctor >/dev/null ||
      fail "doctor --repair failed ($mode)"
    [ "$RUSTUP_TOOLCHAIN" = caller-toolchain ] || fail "repair changed the caller's toolchain ($mode)"
    "$HOME/.local/bin/oms" list >/dev/null || fail "oms list failed after repair ($mode)"
    broken_found=0
    for broken_dir in "$case_dir/checkout/target/oms-native/.broken-$rev1"-*; do
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
