#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tool-lock.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
fail() { echo "tool-lock-smoke: $*" >&2; exit 1; }

LOCK="$ROOT/tools.lock.json"
HELPER="$ROOT/scripts/lib/tool-lock.py"
[ -f "$LOCK" ] || fail "missing tools.lock.json"
[ -x "$HELPER" ] || fail "missing executable tool-lock helper"
python3 "$HELPER" --lock "$LOCK" validate >/dev/null || fail "tool lock is invalid"

# These cases also run alone so admission can prove each against the old installer.
provider_vendor_case() (
  local scenario="$1" tool path prefix digest before
  export HOME="$TMP/vendor-$scenario" PATH=/usr/bin:/bin
  export CODEX_HOME="$HOME/.codex"
  unset OH_MY_SETTING_TOOL_LOCK
  export TMPDIR="$HOME/scratch"
  mkdir -p "$HOME/.local/bin" "$TMPDIR"
  set --
  . "$ROOT/scripts/install-tools.sh"
  export PATH="$HOME/.local/bin:$PATH"
  npm() { printf 'npm %s\n' "$*" >> "$HOME/unexpected"; return 97; }
  download_locked() { printf 'download %s\n' "$1" >> "$HOME/unexpected"; return 98; }
  make_vendor_binary() {
    mkdir -p "$(dirname "$1")"
    printf '#!/usr/bin/env bash\nset -eu\ntool=%q\n' "$2" > "$1"
    cat >> "$1" <<'EOF_VENDOR'
if [ "$tool" = agy ]; then printf '%s\n' "$*" >> "$HOME/agy-invocations"; fi
case "${1:-}" in
  --version)
    if [ "${OMS_TEST_RECORD_VERSIONS:-0}" = 1 ]; then printf '%s\n' "$0" >> "$HOME/version-probes"; fi
    [ "${OMS_TEST_VERSION_FAIL:-0}" != 1 ] || exit 1
    printf '%s %s\n' "$tool" "$(cat "$HOME/$tool.version")" ;;
  update|install)
    printf '%s %s\n' "$tool" "$*" >> "$HOME/calls"
    [ "${OMS_TEST_UPDATE_FAIL:-0}" != 1 ] || exit 1
    version="${OMS_TEST_NEXT:-2.0.0}"
    if [ "$1" = install ]; then
      printf '%s\n' "$0" > "$HOME/last-installer"
      if [ -e "$HOME/.local/bin/claude" ] && [ ! -L "$HOME/.local/bin/claude" ]; then exit 1; fi
      [ "${2:-latest}" = latest ] || version="$2"
      target="$HOME/.local/share/claude/versions/$version"
      mkdir -p "$(dirname "$target")"
      [ "$0" = "$target" ] || cp "$0" "$target"
      ln -sf "$target" "$HOME/.local/bin/claude"
    fi
    printf '%s\n' "$version" > "$HOME/$tool.version"
    if [ "$tool" = agy ] && [ "${OMS_TEST_CHANGE_BINARY:-0}" = 1 ]; then printf '# vendor update\n' >> "$0"; fi
    if [ "${OMS_TEST_ESCAPE:-0}" = 1 ]; then
      cp "$0" "$HOME/foreign"
      ln -sf "$HOME/foreign" "$HOME/.local/bin/$tool"
    fi ;;
  *) exit 1 ;;
esac
EOF_VENDOR
    chmod +x "$1"
    printf '%s\n' "$3" > "$HOME/$2.version"
  }
  add_vendor() {
    case "$1" in
      claude) path="$HOME/.local/share/claude/versions/1.0.0" ;;
      codex) path="$CODEX_HOME/packages/standalone/releases/1.0.0/bin/codex" ;;
      pi)
        path="$HOME/.pi/agent/bin/pi"
        mkdir -p "$HOME/.pi/agent/install/releases/1.0.0"
        printf '1.0.0\n' > "$HOME/.pi/agent/install/current-version" ;;
      agy) path="$HOME/.local/bin/agy" ;;
    esac
    make_vendor_binary "$path" "$1" 1.0.0
    [ "$1" = agy ] || ln -sf "$path" "$HOME/.local/bin/$1"
  }
  case "$scenario" in
    claude-native|codex-channel)
      tool=claude
      [ "$scenario" != codex-channel ] || tool=codex
      add_vendor "$tool"
      before="$(readlink "$HOME/.local/bin/$tool")"
      preflight_npm_shim "$tool" || fail "$tool vendor preflight failed"
      install_npm_global "$tool" || fail "$tool vendor refresh failed"
      write_npm_shim "$tool" || fail "$tool shim bypass failed"
      verify_resolved_npm_binary "$tool" || fail "$tool vendor verification failed"
      grep -Fxq "$tool update" "$HOME/calls" || fail "$tool updater missing"
      [ "$(readlink "$HOME/.local/bin/$tool")" = "$before" ] || fail "$tool launcher changed"
      # Equal versions still use the vendor updater; snapshots cannot force a downgrade.
      install_npm_global "$tool" || fail "$tool equal channel version failed"
      if [ "$tool" = codex ]; then
        before="$(cat "$HOME/calls")"
        export OH_MY_SETTING_TOOL_LOCK="$LOCK"
        if install_npm_global codex > /dev/null 2>&1; then fail "Codex mismatched pin changed"; fi
        tool_lock_get npm.codex.version > "$HOME/codex.version"
        install_npm_global codex || fail "Codex equal pin refused"
        [ "$(cat "$HOME/calls")" = "$before" ] || fail "Codex explicit pin called updater"
        unset OH_MY_SETTING_TOOL_LOCK
        printf '2.0.0\n' > "$HOME/codex.version"
      fi
      export OMS_TEST_NEXT=invalid
      if install_npm_global "$tool" > /dev/null 2>&1; then fail "$tool malformed updated version accepted"; fi
      printf '2.0.0\n' > "$HOME/$tool.version"
      export OMS_TEST_NEXT=0.0.0
      if install_npm_global "$tool" > /dev/null 2>&1; then fail "$tool downgrade accepted"; fi
      printf '1.0.0\n' > "$HOME/$tool.version"
      export OMS_TEST_NEXT=2.0.0 OMS_TEST_UPDATE_FAIL=1
      if install_npm_global "$tool" > /dev/null 2>&1; then fail "$tool updater failure accepted"; fi
      unset OMS_TEST_UPDATE_FAIL
      export OMS_TEST_ESCAPE=1
      if install_npm_global "$tool" > /dev/null 2>&1; then fail "$tool escaped vendor root"; fi ;;
    claude-pin)
      add_vendor claude
      export OH_MY_SETTING_TOOL_LOCK="$LOCK"
      install_npm_global claude || fail "native Claude pin failed"
      grep -Fxq "claude install $(tool_lock_get npm.claude.version)" "$HOME/calls" || fail "Claude pin not installed"
      verify_resolved_npm_binary claude || fail "native Claude pin verification failed"
      export OMS_TEST_VERSION_FAIL=1
      if install_npm_global claude > /dev/null 2>&1; then fail "broken version probe accepted"; fi ;;
    claude-fresh|claude-checksum)
      make_vendor_binary "$HOME/payload" claude 0.0.0
      digest="$(sha256_file "$HOME/payload")"
      [ "$scenario" != claude-checksum ] || digest="$(printf '%064d' 0)"
      download_locked() {
        case "$1" in
          */manifest.json) printf '{"platforms":{"linux-x64":{"checksum":"%s"}}}\n' "$digest" > "$2" ;;
          */linux-x64/claude) cp "$HOME/payload" "$2" ;;
          *) return 1 ;;
        esac
      }
      locked_platform() { printf 'linux-amd64\n'; }
      oms_platform_is_windows() { return 1; }
      if [ "$scenario" = claude-checksum ]; then
        if install_npm_global claude > "$HOME/error" 2>&1; then fail "Claude checksum mismatch accepted"; fi
        grep -Fq 'checksum mismatch' "$HOME/error" || fail "Claude checksum failure missing"
        for digest in '' invalid; do
          if install_npm_global claude > "$HOME/error" 2>&1; then fail "malformed Claude manifest accepted"; fi
          grep -Fq 'invalid Claude manifest checksum' "$HOME/error" || fail "manifest refusal missing"
        done
        download_locked() { printf '[]\n' > "$2"; }
        if install_npm_global claude > "$HOME/error" 2>&1; then fail "wrong manifest shape accepted"; fi
        [ ! -e "$HOME/.local/bin/claude" ] && [ ! -e "$HOME/calls" ] || fail "unverified Claude executed"
      else
        preflight_npm_shim claude || fail "fresh Claude required npm"
        install_npm_global claude || fail "fresh native Claude failed"
        grep -Fxq "claude install $(tool_lock_get npm.claude.version)" "$HOME/calls" || fail "fresh native install missing"
        verify_resolved_npm_binary claude || fail "fresh Claude verification failed"
      fi ;;
    claude-migration)
      add_vendor claude
      rm "$HOME/.local/bin/claude"
      prefix="$HOME/oms-node"
      mkdir -p "$prefix/bin" "$prefix/lib/node_modules/@anthropic-ai/claude-code"
      printf '%s\n' '{"name":"@anthropic-ai/claude-code","version":"1.0.0","bin":{"claude":"cli.js"}}' > "$prefix/lib/node_modules/@anthropic-ai/claude-code/package.json"
      make_vendor_binary "$prefix/lib/node_modules/@anthropic-ai/claude-code/cli.js" claude 1.0.0
      ln -s ../lib/node_modules/@anthropic-ai/claude-code/cli.js "$prefix/bin/claude"
      printf '#!/usr/bin/env bash\nset -euo pipefail\n\n# managed by oh-my-setting; rewritten on every tool update\nexport PATH=%q:"$PATH"\nexec %q "$@"\n' "$prefix/bin" "$prefix/bin/claude" > "$HOME/.local/bin/claude"
      chmod +x "$HOME/.local/bin/claude"
      cp "$HOME/.local/bin/claude" "$HOME/original-shim"
      export PATH="$prefix/bin:$PATH"
      cat > "$prefix/bin/npm" <<'EOF_NPM_VENDOR'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$HOME/npm-calls"
case "$*" in
  *uninstall*"$HOME/oms-node"*)
    [ "${OMS_TEST_NPM_FAIL:-0}" != 1 ] || exit 1
    rm -rf "$HOME/oms-node/lib/node_modules/@anthropic-ai/claude-code"
    rm -f "$HOME/oms-node/bin/claude" ;;
  *) exit 1 ;;
esac
EOF_NPM_VENDOR
      chmod +x "$prefix/bin/npm"
      mkdir -p "$HOME/other-prefix/lib/node_modules/@anthropic-ai/claude-code"
      rm "$HOME/.local/share/claude/versions/1.0.0"
      npm_global_root() { printf '%s\n' "$HOME/oms-node/lib/node_modules"; }
      npm_global_bin_dir() { printf '%s\n' "$HOME/oms-node/bin"; }
      npm_package_is_current() { printf 'npm channel\n' >> "$HOME/npm-current"; return 0; }
      for native_state in absent empty malformed nonexecutable outside; do
        case "$native_state" in
          absent) rmdir "$HOME/.local/share/claude/versions" ;;
          empty) mkdir -p "$HOME/.local/share/claude/versions" ;;
          malformed) printf 'not a release\n' > "$HOME/.local/share/claude/versions/latest" ;;
          nonexecutable) printf 'not executable\n' > "$HOME/.local/share/claude/versions/0.0.0" ;;
          outside) ln -s "$prefix/lib/node_modules/@anthropic-ai/claude-code/cli.js" "$HOME/.local/share/claude/versions/1.1.0" ;;
        esac
        if claude_migration_pending; then fail "$native_state native directory triggered migration"; fi
        OH_MY_SETTING_TOOL_LOCK="$LOCK" prepare_provider_versions claude || fail "$native_state native directory blocked preparation"
        preflight_npm_shim claude || fail "$native_state native directory blocked shim preflight"
        install_npm_global claude > /dev/null || fail "$native_state native directory blocked npm channel"
      done
      [ "$(wc -l < "$HOME/npm-current" | tr -d ' ')" = 5 ] || fail "incomplete native directory skipped npm channel"
      [ ! -e "$HOME/calls" ] || fail "incomplete native directory ran native installer"
      unset -f npm_package_is_current
      make_vendor_binary "$HOME/.local/share/claude/versions/0.0.0" claude 1.0.0
      claude_migration_pending || fail "zero native version was not a valid candidate"
      make_vendor_binary "$HOME/.local/share/claude/versions/1.9.0" claude 1.0.0
      make_vendor_binary "$HOME/.local/share/claude/versions/1.10.0" claude 1.0.0
      preflight_npm_shim claude || fail "migration preflight failed"
      cp "$HOME/.local/bin/claude" "$HOME/.local/bin/.claude.oh-my-setting-migration"
      if migrate_claude_vendor > /dev/null 2>&1; then fail "conflicting migration backup accepted"; fi
      [ ! -e "$HOME/calls" ] || fail "conflicting backup invoked vendor installer"
      rm "$HOME/.local/bin/.claude.oh-my-setting-migration"
      export OMS_TEST_UPDATE_FAIL=1
      if install_npm_global claude > /dev/null 2>&1; then fail "migration installer failure ignored"; fi
      npm_shim_owned "$HOME/.local/bin/claude" || fail "failed migration lost OMS shim"
      unset OMS_TEST_UPDATE_FAIL
      # Simulate a kill after the atomic move and resume without an active launcher.
      mv "$HOME/.local/bin/claude" "$HOME/.local/bin/.claude.oh-my-setting-migration"
      export OMS_TEST_NPM_FAIL=1
      if install_npm_global claude > "$HOME/error" 2>&1; then fail "npm cleanup failure ignored"; fi
      [ -L "$HOME/.local/bin/claude" ] && [ -f "$HOME/.local/bin/.claude.oh-my-setting-migration" ] || fail "partial migration lost recovery state"
      grep -Fq uninstall "$HOME/npm-calls" || fail "migration verified the npm PATH copy instead of the native launcher"
      before="$(grep -c '^claude install ' "$HOME/calls")"
      unset OMS_TEST_NPM_FAIL
      install_npm_global claude || fail "Claude migration retry failed"
      [ "$(grep -c '^claude install ' "$HOME/calls")" = "$before" ] || fail "partial migration installed Claude again"
      [ -L "$HOME/.local/bin/claude" ] || fail "OMS shim survived migration"
      [ "$(cat "$HOME/last-installer")" = "$HOME/.local/share/claude/versions/1.10.0" ] || fail "migration did not select newest native version"
      [ ! -e "$HOME/.local/bin/.claude.oh-my-setting-migration" ] || fail "migration backup survived success"
      [ ! -d "$prefix/lib/node_modules/@anthropic-ai/claude-code" ] || fail "OMS npm copy survived migration"
      [ -d "$HOME/other-prefix/lib/node_modules/@anthropic-ai/claude-code" ] || fail "unrelated npm prefix removed"
      install_npm_global claude || fail "second migrated update failed"
      [ "$(wc -l < "$HOME/npm-calls" | tr -d ' ')" = 2 ] || fail "migration repeated npm removal"
      # A kill after npm cleanup leaves only the parked shim to finalize.
      cp "$HOME/original-shim" "$HOME/.local/bin/.claude.oh-my-setting-migration"
      before="$(grep -c '^claude install ' "$HOME/calls")"
      install_npm_global claude || fail "post-cleanup migration did not finalize"
      [ "$(grep -c '^claude install ' "$HOME/calls")" = "$before" ] || fail "post-cleanup migration installed Claude again"
      [ ! -e "$HOME/.local/bin/.claude.oh-my-setting-migration" ] || fail "post-cleanup migration left recovery state"
      # A malformed ownership record refuses before the native installer runs.
      rm "$HOME/.local/bin/claude"
      printf '#!/usr/bin/env bash\nset -euo pipefail\n\n# managed by oh-my-setting; rewritten on every tool update\nexit 0\n' > "$HOME/.local/bin/claude"
      before="$(cat "$HOME/calls")"
      if migrate_claude_vendor > /dev/null 2>&1; then fail "unproven migration accepted"; fi
      [ "$(cat "$HOME/calls")" = "$before" ] || fail "unproven migration changed native install" ;;
    claude-local-npm)
      prefix="$HOME/.local"
      mkdir -p "$prefix/lib/node_modules/@anthropic-ai/claude-code"
      printf '%s\n' '{"name":"@anthropic-ai/claude-code","version":"1.0.0","bin":{"claude":"cli.js"}}' > "$prefix/lib/node_modules/@anthropic-ai/claude-code/package.json"
      make_vendor_binary "$prefix/lib/node_modules/@anthropic-ai/claude-code/cli.js" claude 1.0.0
      ln -s ../lib/node_modules/@anthropic-ai/claude-code/cli.js "$prefix/bin/claude"
      npm_global_root() { printf '%s\n' "$HOME/.local/lib/node_modules"; }
      npm_global_bin_dir() { (cd "$HOME/.local/bin" && pwd -P); }
      export OH_MY_SETTING_TOOL_LOCK="$LOCK"
      preflight_npm_shim claude || fail "local npm Claude launcher was refused"
      prepare_provider_versions claude || fail "local npm Claude blocked profile reapply"
      if native_claude; then fail "local npm Claude was mistaken for vendor layout"; fi
      [ -L "$prefix/bin/claude" ] || fail "local npm Claude launcher changed"
      ln -s "$HOME" "$TMP/local-npm-home-alias"
      HOME="$TMP/local-npm-home-alias" preflight_npm_shim claude || fail "physical local npm prefix alias was refused"
      HOME="$TMP/local-npm-home-alias" write_npm_shim claude || fail "npm-owned local launcher was rewritten through HOME alias"
      printf '{}\n' > "$prefix/lib/node_modules/@anthropic-ai/claude-code/package.json"
      if preflight_npm_shim claude > /dev/null 2>&1; then fail "malformed local npm identity accepted"; fi
      printf '%s\n' '{"name":"@anthropic-ai/claude-code","version":"1.0.0","bin":{"claude":"cli.js"}}' > "$prefix/lib/node_modules/@anthropic-ai/claude-code/package.json"
      npm_global_root() { printf '\n'; }
      if preflight_npm_shim claude > /dev/null 2>&1; then fail "empty active npm root accepted"; fi
      npm_global_root() { printf '%s\n' "$HOME/missing-root"; }
      if preflight_npm_shim claude > /dev/null 2>&1; then fail "missing active npm root accepted"; fi
      npm_global_root() { printf '%s\n' "$HOME/.local/lib/node_modules"; }
      rm "$prefix/bin/claude"
      ln -s "$HOME/missing" "$prefix/bin/claude"
      if preflight_npm_shim claude > /dev/null 2>&1; then fail "dangling local npm launcher accepted"; fi ;;
    claude-unowned)
      npm_global_root() { return 1; }
      make_vendor_binary "$HOME/external" claude 1.0.0
      ln -s "$HOME/external" "$HOME/.local/bin/claude"
      add_vendor agy
      before="$(readlink "$HOME/.local/bin/claude")"
      if OH_MY_SETTING_TOOL_LOCK="$LOCK" prepare_provider_versions claude > "$HOME/error" 2>&1; then fail "foreign launcher passed profile preflight"; fi
      if preflight_npm_shim claude > "$HOME/error" 2>&1; then fail "foreign launcher reached npm preflight"; fi
      if install_npm_global claude > "$HOME/error" 2>&1; then fail "foreign Claude changed"; fi
      [ "$(readlink "$HOME/.local/bin/claude")" = "$before" ] || fail "foreign launcher moved"
      [ ! -e "$HOME/calls" ] || fail "foreign Claude executed updater"
      # A dangling launcher must also refuse, rather than falling through to fresh install.
      rm "$HOME/external"
      if install_npm_global claude > "$HOME/error" 2>&1; then fail "dangling Claude overwritten"; fi ;;
    pi|agy)
      add_vendor "$scenario"
      if [ "$scenario" = pi ]; then
        refresh_optional_providers || fail "pi native refresh failed"
        grep -Fxq 'pi update self' "$HOME/calls" || fail "pi native updater missing"
        before="$(grep -c '^pi update self$' "$HOME/calls")"
        XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" \
          bash "$ROOT/scripts/install-tools.sh" --providers > "$HOME/pi-refresh" || fail "installed Pi --providers refresh failed"
        [ "$(grep -c '^pi update self$' "$HOME/calls")" = "$((before + 1))" ] || fail "--providers did not refresh installed Pi"
        for document in INSTALL.md COMPONENTS.md; do
          grep -Fq 'Pi refresh runs only through `install-tools.sh --providers`' "$ROOT/docs/$document" || fail "Pi lifecycle docs promise unsupported refresh paths"
        done
        grep -Fq 'Pi 갱신은 `install-tools.sh --providers`에서만' "$ROOT/docs/INSTALL.ko.md" || fail "Korean Pi lifecycle docs promise unsupported refresh paths"
        bash "$ROOT/scripts/install-tools.sh" --help | grep -Fq 'Only --providers refreshes installed Pi' || fail "Pi help promises unsupported refresh paths"
      else
        direct_owner_write "$HOME/.local/bin/agy"
        cp "$HOME/.local/bin/agy" "$HOME/agy-original"
        printf '# unexpected bytes\n' >> "$HOME/.local/bin/agy"
        mkdir -p "$HOME/agy-alias"
        ln -s "$HOME/.local/bin/agy" "$HOME/agy-alias/agy"
        export PATH="$HOME/agy-alias:$PATH"
        . "$ROOT/scripts/lib/doctor-probe-memo.sh"
        for function_name in normalize_doctor_npm_path doctor_command_digest_ok doctor_sha256_file doctor_provider_inventory check_vendor_provider_version provider_lock_path command_has_locked_version report_tool_drift check_locked_direct_version; do
          eval "$(sed -n "/^${function_name}()/,/^}/p" "$ROOT/scripts/doctor.sh")"
        done
        INSTALL_ROOT="$ROOT"
        REQUIRE_TOOLS=1
        tool_lock_value() { tool_lock_get "$1"; }
        for pin_mode in channel pinned; do
          if [ "$pin_mode" = pinned ]; then export OH_MY_SETTING_TOOL_LOCK="$LOCK"; else unset OH_MY_SETTING_TOOL_LOCK; fi
          FAILED=0
          check_locked_direct_version agy agy antigravity.version > "$HOME/agy-doctor"
          [ "$FAILED" = 1 ] || fail "doctor accepted changed agy bytes in $pin_mode mode"
          grep -Fq 'managed binary digest changed' "$HOME/agy-doctor" || fail "doctor failed to name agy integrity drift"
        done
        unset OH_MY_SETTING_TOOL_LOCK
        doctor_result=0
        XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" \
          OMS_INSTALL_RECEIPT="$HOME/.config/oh-my-setting/install.json" \
          OH_MY_SETTING_REQUIRE_TOOLS=1 OH_MY_SETTING_MODEL_DOCTOR=auto \
          OMS_AGY_BIN="$HOME/.local/bin/agy" OH_MY_SETTING_AGY_PLUGIN=0 \
          bash "$ROOT/scripts/doctor.sh" > "$HOME/complete-doctor" 2>&1 || doctor_result=$?
        [ "$doctor_result" != 0 ] || fail "complete doctor accepted tampered agy"
        grep -Fq 'managed binary digest changed' "$HOME/complete-doctor" || fail "complete doctor omitted integrity drift"
        [ ! -s "$HOME/agy-invocations" ] || fail "complete doctor executed tampered agy before its digest check"
        add_vendor claude
        XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" \
          OMS_INSTALL_RECEIPT="$HOME/.config/oh-my-setting/install.json" \
          OH_MY_SETTING_REQUIRE_TOOLS=0 OH_MY_SETTING_MODEL_DOCTOR=auto \
          OMS_AGY_BIN="$HOME/.local/bin/agy" OH_MY_SETTING_AGY_PLUGIN=0 \
          bash "$ROOT/scripts/doctor.sh" > "$HOME/optional-doctor" 2>&1 || true
        [ ! -s "$HOME/agy-invocations" ] || fail "warning-only doctor executed tampered agy: $(cat "$HOME/agy-invocations")"
        grep -Fq 'unsafe probes skipped' "$HOME/optional-doctor" || fail "warning-only doctor omitted skipped probes"
        grep -Fq 'tool versions not probed' "$HOME/optional-doctor" || fail "warning-only doctor disguised skipped probes as version failures"
        if grep -Eq 'tool version drift: (node|claude|codex|ntn)|vendor --version failed' "$HOME/optional-doctor"; then fail "skipped probes reported unrelated tool drift"; fi
        before="$(cat "$HOME/.local/bin/agy.oh-my-setting-managed")"
        if install_antigravity > "$HOME/agy-error" 2>&1; then fail "changed agy bytes reached vendor updater"; fi
        grep -Fq 'managed binary digest changed' "$HOME/agy-error" || fail "installer failed to name agy integrity drift"
        grep -Fq 'restore' "$HOME/agy-error" || fail "agy integrity refusal has no recovery"
        [ ! -e "$HOME/calls" ] || fail "tampered agy update was executed"
        [ "$(cat "$HOME/.local/bin/agy.oh-my-setting-managed")" = "$before" ] || fail "tampered agy was re-blessed"
        cp "$HOME/agy-original" "$HOME/.local/bin/agy"
        cp "$HOME/.local/bin/agy" "$HOME/.local/bin/agy.oh-my-setting-backup"
        printf '# verified replacement\n' >> "$HOME/.local/bin/agy"
        tool_lock_get antigravity.version > "$HOME/agy.version"
        python3 - "$HOME/.local/bin/agy" "$HOME/agy-verified.tar.gz" <<'PY_AGY_ARCHIVE'
import sys, tarfile
with tarfile.open(sys.argv[2], "w:gz") as archive:
    archive.add(sys.argv[1], arcname="antigravity")
PY_AGY_ARCHIVE
        locked_platform() { printf 'linux-amd64\n'; }
        tool_lock_get() {
          case "$1" in
            antigravity.platforms.*.sha512)
              if [ "${OMS_TEST_BAD_RECOVERY:-0}" = 1 ]; then printf '%0128d\n' 0; else sha512_file "$HOME/agy-verified.tar.gz"; fi ;;
            antigravity.platforms.*.archive) printf 'tar.gz\n' ;;
            *) oms_lock_scalar "$TOOL_LOCK" "$1" ;;
          esac
        }
        download_locked() { printf 'archive\n' >> "$HOME/agy-downloads"; cp "$HOME/agy-verified.tar.gz" "$2"; }
        before="$(cat "$HOME/.local/bin/agy.oh-my-setting-managed")"
        if (export OMS_TEST_BAD_RECOVERY=1; install_antigravity) > "$HOME/bad-recovery" 2>&1; then fail "unverified agy recovery archive accepted"; fi
        [ "$(cat "$HOME/.local/bin/agy.oh-my-setting-managed")" = "$before" ] && [ -f "$HOME/.local/bin/agy.oh-my-setting-backup" ] || fail "bad recovery archive changed transaction state"
        [ ! -s "$HOME/agy-invocations" ] || fail "agy recovery executed bytes before checksum verification"
        OH_MY_SETTING_TOOL_LOCK="$LOCK" install_antigravity || fail "verified interrupted agy swap was not recovered"
        direct_target_owned "$HOME/.local/bin/agy" || fail "recovered agy owner digest was not committed"
        [ ! -e "$HOME/.local/bin/agy.oh-my-setting-backup" ] && [ ! -e "$HOME/.local/bin/agy.oh-my-setting-stage" ] || fail "agy recovery left transaction residue"
        for function_name in tool_lock_get locked_platform; do
          eval "$(sed -n "/^${function_name}()/,/^}/p" "$ROOT/scripts/install-tools.sh")"
        done
        rm -f "$HOME/agy-invocations"
        cp "$HOME/.local/bin/agy" "$HOME/override-agy"
        direct_owner_write "$HOME/override-agy"
        printf '# changed override bytes\n' >> "$HOME/override-agy"
        doctor_result=0
        XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" \
          OMS_INSTALL_RECEIPT="$HOME/.config/oh-my-setting/install.json" \
          OMS_DOCTOR_INTEGRATIONS_ONLY=1 OMS_AGY_BIN="$HOME/override-agy" \
          OH_MY_SETTING_CLAUDE_HOOKS=0 OH_MY_SETTING_CODEX_PLUGIN=0 \
          OH_MY_SETTING_MCP=0 OH_MY_SETTING_AGY_PLUGIN=1 \
          bash "$ROOT/scripts/doctor.sh" > "$HOME/override-doctor" 2>&1 || doctor_result=$?
        [ "$doctor_result" != 0 ] || fail "doctor accepted a changed provider override"
        [ ! -s "$HOME/agy-invocations" ] || fail "doctor executed a marked provider override before verification"
        grep -Fq 'managed binary digest changed' "$HOME/override-doctor" || fail "provider override drift was omitted"
        mv "$HOME/.local/bin/agy.oh-my-setting-managed" "$HOME/agy-saved-marker"
        : > "$HOME/.local/bin/agy.oh-my-setting-stage"
        if install_antigravity > "$HOME/unowned-recovery" 2>&1; then fail "unowned agy residue authorized recovery"; fi
        [ ! -s "$HOME/agy-invocations" ] || fail "unowned recovery executed agy"
        rm "$HOME/.local/bin/agy.oh-my-setting-stage"
        mv "$HOME/agy-saved-marker" "$HOME/.local/bin/agy.oh-my-setting-managed"
        FAILED=0
        export OMS_TEST_CHANGE_BINARY=1
        before="$(sha256_file "$HOME/.local/bin/agy")"
        install_antigravity || fail "agy native refresh failed"
        grep -Fxq 'agy update' "$HOME/calls" || fail "agy updater missing"
        [ "$(sha256_file "$HOME/.local/bin/agy")" != "$before" ] || fail "agy fixture did not replace bytes"
        direct_target_owned "$HOME/.local/bin/agy" || fail "agy updater left stale ownership digest"
        FAILED=0
        check_locked_direct_version agy agy antigravity.version > "$HOME/agy-updated-doctor"
        [ "$FAILED" = 0 ] || fail "valid updated agy was forced back to snapshot version"
        for residue in stage backup; do
          residue_path="$HOME/.local/bin/agy.oh-my-setting-$residue"
          for residue_kind in empty dangling; do
            if [ "$residue_kind" = empty ]; then : > "$residue_path"; else ln -s "$HOME/missing-residue" "$residue_path"; fi
            FAILED=0
            check_locked_direct_version agy agy antigravity.version > "$HOME/agy-residue-doctor"
            [ "$FAILED" = 1 ] || fail "doctor accepted agy $residue_kind $residue residue"
            grep -Fq 'interrupted direct-tool transaction residue is present' "$HOME/agy-residue-doctor" || fail "doctor omitted agy transaction residue"
            rm "$residue_path"
          done
        done
        [ "$(wc -l < "$HOME/agy-downloads" | tr -d ' ')" = 2 ] || fail "ordinary vendor refresh re-downloaded agy"
      fi
      before="$(cat "$HOME/calls")"
      export OH_MY_SETTING_TOOL_LOCK="$LOCK"
      if [ "$scenario" = pi ]; then
        refresh_optional_providers > "$HOME/pi-note" || fail "explicit lock plus native Pi blocked refresh"
        [ "$(wc -l < "$HOME/pi-note" | tr -d ' ')" = 1 ] || fail "locked Pi skip was noisy"
        grep -Fq 'pi refresh skipped' "$HOME/pi-note" || fail "locked Pi skip note missing"
        XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache" \
          bash "$ROOT/scripts/install-tools.sh" --providers > "$HOME/pi-providers" || fail "locked Pi failed --providers"
        grep -Fq 'pi refresh skipped' "$HOME/pi-providers" || fail "--providers missed locked Pi note"
      else
        if install_antigravity > /dev/null 2>&1; then fail "agy mismatched pin refreshed"; fi
      fi
      [ "$(cat "$HOME/calls")" = "$before" ] || fail "explicit pin mutated $scenario"
      if [ "$scenario" = pi ]; then
        unset OH_MY_SETTING_TOOL_LOCK
        prefix="$HOME/optional"
        mkdir -p "$prefix/root/cursor-pkg" "$prefix/bin"
        printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli"}}' > "$prefix/root/cursor-pkg/package.json"
        printf '#!/bin/sh\nexit 0\n' > "$prefix/root/cursor-pkg/cli"
        chmod +x "$prefix/root/cursor-pkg/cli"
        ln -s ../root/cursor-pkg/cli "$prefix/bin/cursor-agent"
        export PATH="$prefix/bin:$PATH"
        npm_global_root() { printf '%s\n' "$HOME/optional/root"; }
        npm_global_prefix() { printf '%s\n' "$HOME/optional"; }
        npm_global_bin_dir() { printf '%s\n' "$HOME/optional/bin"; }
        optional_npm_target() { return 0; }
        id() { if [ "$1" = -u ]; then printf '1000\n'; else command id "$@"; fi; }
        npm_transaction_begin "$prefix/root/cursor-pkg" "$prefix/bin" cursor-agent cursor-agent || fail "recovery fixture failed"
        refresh_optional_providers || fail "vendor refresh lost interrupted optional npm recovery"
        [ -L "$prefix/bin/cursor-agent" ] && [ -f "$prefix/root/cursor-pkg/package.json" ] || fail "missing optional command was not recovered"
        [ ! -e "$prefix/root/cursor-pkg.oh-my-setting-transaction" ] || fail "optional recovery left its marker"
      fi
      if [ "$scenario" = agy ]; then
        tool_lock_get antigravity.version > "$HOME/agy.version"
        install_antigravity || fail "equal agy pin refused"
        [ "$(cat "$HOME/calls")" = "$before" ] || fail "equal agy pin invoked updater"
        unset OH_MY_SETTING_TOOL_LOCK
        export OMS_TEST_NEXT=0.0.0
        if install_antigravity > /dev/null 2>&1; then fail "agy downgrade accepted"; fi
        before="$(cat "$HOME/calls")"
        for marker_value in '' malformed; do
          printf '%s\n' "$marker_value" > "$HOME/.local/bin/agy.oh-my-setting-managed"
          if install_antigravity > /dev/null 2>&1; then fail "malformed agy ownership marker accepted"; fi
          [ "$(cat "$HOME/calls")" = "$before" ] || fail "invalid agy marker was checked after mutation"
        done
      fi ;;
    vendor-refresh)
      for tool in claude codex agy pi; do add_vendor "$tool"; done
      for tool in npm curl; do
        printf '#!/usr/bin/env bash\nprintf "unexpected %%s\\n" "$*" >> "$HOME/unexpected"\nexit 1\n' > "$HOME/.local/bin/$tool"
        chmod +x "$HOME/.local/bin/$tool"
      done
      export XDG_CONFIG_HOME="$HOME/.config" XDG_CACHE_HOME="$HOME/.cache"
      python3() {
        if [ "$1" = "$ROOT/scripts/lib/provider-latest.py" ]; then
          printf 'unexpected release lookup\n' >> "$HOME/unexpected"
          return 96
        fi
        command python3 "$@"
      }
      export -f python3
      bash "$ROOT/scripts/install-tools.sh" --providers > "$HOME/refresh" || fail "vendor-only refresh required npm/network"
      [ ! -e "$HOME/.local/bin/agy.oh-my-setting-managed" ] || fail "vendor agy acquired OMS ownership"
      for tool in claude codex agy; do grep -Fxq "$tool update" "$HOME/calls" || fail "$tool missing from --providers"; done
      grep -Fxq 'pi update self' "$HOME/calls" || fail "pi missing from --providers"
      # Physical HOME aliases and native Python CRLF must resolve to the same layout.
      ln -s "$HOME" "$TMP/vendor-alias"
      HOME="$TMP/vendor-alias" native_claude || fail "physical HOME alias rejected"
      python3() { command python3 "$@" | sed 's/$/\r/'; }
      native_claude || fail "native path kept Python carriage returns"
      unset -f python3
      HOME="$TMP/empty-vendor-home"
      export HOME
      mkdir -p "$HOME"
      PATH=/usr/bin:/bin bash "$ROOT/scripts/install-tools.sh" --providers > "$HOME/empty"
      grep -Fxq 'providers: none installed' "$HOME/empty" || fail "provider-free refresh was not quiet"
      [ "$(wc -l < "$HOME/empty" | tr -d ' ')" = 2 ] || fail "provider-free refresh emitted extra output" ;;
    doctor)
      for tool in claude codex agy pi; do add_vendor "$tool"; done
      direct_owner_write "$HOME/.local/bin/agy"
      make_vendor_binary "$HOME/.local/share/claude/versions/0.9.0" claude 1.0.0
      make_vendor_binary "$CODEX_HOME/packages/standalone/releases/0.9.0/bin/codex" codex 1.0.0
      ln -s releases/1.0.0 "$CODEX_HOME/packages/standalone/current"
      INSTALL_ROOT="$ROOT"
      REQUIRE_TOOLS=0
      . "$ROOT/scripts/lib/doctor-probe-memo.sh"
      for tool in normalize_doctor_npm_path doctor_command_digest_ok doctor_sha256_file doctor_provider_inventory check_vendor_provider_version provider_lock_path command_has_locked_version; do
        eval "$(sed -n "/^$tool()/,/^}/p" "$ROOT/scripts/doctor.sh")"
      done
      tool_lock_value() { tool_lock_get "$1"; }
      report_tool_drift() { printf 'drift: %s\n' "$*"; }
      export OMS_TEST_RECORD_VERSIONS=1
      mkdir -p "$HOME/alias-bin"
      ln -s "$HOME/.local/bin/claude" "$HOME/alias-bin/claude"
      export PATH="$PATH:$HOME/alias-bin"
      doctor_provider_inventory > "$HOME/native-report"
      if grep -Fq 'multiple copies of' "$HOME/native-report"; then fail "vendor retention was reported as duplicates"; fi
      [ "$(wc -l < "$HOME/version-probes" | tr -d ' ')" = 4 ] || fail "doctor probed retained versions or repeated aliases"
      if grep -Eq '/(versions|releases)/0.9.0' "$HOME/version-probes"; then fail "doctor executed retained vendor version"; fi
      : > "$HOME/version-probes"
      export OMS_TEST_VERSION_FAIL=1
      doctor_provider_inventory > "$HOME/unavailable-report"
      [ "$(wc -l < "$HOME/version-probes" | tr -d ' ')" = 4 ] || fail "failed version probes were repeated for aliases"
      grep -Fq 'version=unavailable' "$HOME/unavailable-report" || fail "failed version probe was hidden"
      unset OMS_TEST_VERSION_FAIL
      mkdir -p "$HOME/repo/bin"
      cat > "$HOME/repo/bin/codex" <<'EOF_FOREIGN'
#!/bin/sh
printf '%s\n' "$*" >> "$HOME/foreign-invocations"
printf 'codex 1.0.0\n'
EOF_FOREIGN
      chmod +x "$HOME/repo/bin/codex"
      inventory_path="$PATH"
      export PATH="$PATH:$HOME/repo/bin"
      : > "$HOME/version-probes"
      doctor_provider_inventory > "$HOME/foreign-report"
      [ ! -e "$HOME/foreign-invocations" ] || fail "inventory executed an unselected unmarked foreign copy"
      grep -Fq "channel=unknown version=unknown path=$HOME/repo/bin/codex (not executed)" "$HOME/foreign-report" || fail "inventory omitted the unexecuted foreign copy"
      export PATH="$inventory_path"
      if ! oms_platform_is_windows; then
        python3 - <<'PY_PATH_FORGERY'
from pathlib import Path
base = Path.home() / "path-copies"
forged = "\x1f".join(("copy", "codex", "selected", "unknown", "-", "evil", "-", "foreign"))
for index, name in enumerate(("payload\n", "payload\x1f", "payload\r", "payload\n" + forged)):
    directory = base / str(index)
    directory.mkdir(parents=True)
    target = directory / name
    target.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$HOME/path-invocations"\nprintf "codex 1.0.0\\n"\n')
    target.chmod(0o755)
    (directory / "codex").symlink_to(name)
evil = base / "3/evil"
evil.write_bytes(target.read_bytes())
evil.chmod(0o755)
PY_PATH_FORGERY
        for index in 0 1 2 3; do export PATH="$PATH:$HOME/path-copies/$index"; done
        path_directory="$HOME/path-copies/directory"$'\ncopy\037codex\037selected\037unknown\037-\037evil\037-\037foreign'
        mkdir -p "$path_directory"
        ln -s "$HOME/path-copies/3/evil" "$path_directory/codex"
        export PATH="$PATH:$path_directory"
        doctor_provider_inventory > "$HOME/path-report"
        [ ! -e "$HOME/path-invocations" ] || fail "filesystem fields forged an executable selected row"
        for index in 0 1 2 3; do
          grep -Fq "channel=unknown version=unknown path=\"$HOME/path-copies/$index/codex\" (not executed)" "$HOME/path-report" || fail "unsafe filesystem fields were not reported without execution"
        done
        grep -Fq "channel=unknown version=unknown path=\"$HOME/path-copies/directory\\ncopy" "$HOME/path-report" || fail "unsafe launcher or remedy fields were not escaped"
        PATH="$HOME/path-copies/3:$PATH" doctor_provider_inventory > "$HOME/selected-path-report"
        [ ! -e "$HOME/path-invocations" ] || fail "selected copy with unsafe fields executed"
        grep -Fq 'codex selected channel=unknown version=unknown' "$HOME/selected-path-report" || fail "unsafe selected copy was not reported"
        export PATH="$inventory_path"
      else
        echo "note: filesystem separator fixture requires POSIX filenames"
      fi
      make_vendor_binary "$HOME/.local/share/claude/versions/claude" claude 1.0.0
      make_vendor_binary "$HOME/.pi/agent/install/releases/0.9.0/bin/pi" pi 1.0.0
      export PATH="$PATH:$CODEX_HOME/packages/standalone/releases/0.9.0/bin:$HOME/.local/share/claude/versions:$HOME/.pi/agent/install/releases/0.9.0/bin"
      : > "$HOME/version-probes"
      doctor_provider_inventory > "$HOME/retained-report"
      for path in "$CODEX_HOME/packages/standalone/releases/0.9.0/bin/codex" "$HOME/.local/share/claude/versions/claude" "$HOME/.pi/agent/install/releases/0.9.0/bin/pi"; do
        if grep -Fq "rm -f -- $path" "$HOME/retained-report"; then fail "inventory proposed removing a vendor-retained payload"; fi
        grep -Fq "path=$path (not executed)" "$HOME/retained-report" || fail "retained PATH copy was omitted or probed"
      done
      if grep -Fq 'multiple copies of' "$HOME/retained-report"; then fail "retained PATH releases were counted as separate installations"; fi
      [ "$(wc -l < "$HOME/version-probes" | tr -d ' ')" = 4 ] || fail "inventory probed retained PATH releases"
      export PATH="$PATH:$HOME/repo/bin"
      node_bin="$HOME/.nvm/versions/node/v$NODE_VERSION/bin"
      mkdir -p "$node_bin"
      printf '#!/bin/sh\ntouch "$HOME/node-probed"\necho v%s\n' "$NODE_VERSION" > "$node_bin/node"
      chmod +x "$node_bin/node"
      direct_owner_write "$node_bin/node"
      printf '# changed bytes\n' >> "$node_bin/node"
      eval "$(sed -n '/^load_user_tool_paths()/,/^}/p' "$ROOT/scripts/doctor.sh")"
      load_user_tool_paths > "$HOME/node-report"
      [ ! -e "$HOME/node-probed" ] || fail "doctor executed marked node before checking its digest"
      grep -Fq 'managed binary digest changed' "$HOME/node-report" || fail "doctor skipped node without reporting drift"
      [ "${DOCTOR_SKIP_TOOL_PROBES:-0}" = 1 ] || fail "changed bootstrap node did not suppress dependent probes"
      DOCTOR_SKIP_TOOL_PROBES=0
      prefix="$HOME/.nvm/versions/node/v1/lib/node_modules/@anthropic-ai/claude-code"
      mkdir -p "$prefix"
      printf '%s\n' '{"name":"@anthropic-ai/claude-code","version":"0.5.0"}' > "$prefix/package.json"
      for tool in '@openai/codex' '@earendil-works/pi-coding-agent'; do
        prefix="$HOME/.nvm/versions/node/v2/lib/node_modules/$tool"
        mkdir -p "$prefix"
        printf '{"name":"%s","version":"0.6.0"}\n' "$tool" > "$prefix/package.json"
      done
      empty_package="$HOME/.nvm/versions/node/v3/lib/node_modules/@anthropic-ai/claude-code"
      mkdir -p "$empty_package"
      printf '%s\n' '{"name":"@anthropic-ai/claude-code","version":""}' > "$empty_package/package.json"
      malformed_package="$HOME/.nvm/versions/node/v4/lib/node_modules/@openai/codex"
      mkdir -p "$malformed_package"
      python3 - "$malformed_package/package.json" <<'PY_BAD_METADATA'
import json, sys
from pathlib import Path
forged = "\x1f".join(("copy", "codex", "selected", "unknown", "-", str(Path.home() / "repo/bin/codex"), "-", "foreign"))
Path(sys.argv[1]).write_text(json.dumps(dict(name="@openai/codex", version="0.0.0\n" + forged + "\n0.0.0")))
PY_BAD_METADATA
      make_vendor_binary "$HOME/foreign-bin/claude" claude 1.0.0
      direct_owner_write "$HOME/foreign-bin/claude"
      printf '# changed bytes\n' >> "$HOME/foreign-bin/claude"
      marked_package="$HOME/foreign-npm/root/@openai/codex"
      mkdir -p "$marked_package" "$HOME/foreign-npm/bin"
      printf '%s\n' '{"name":"@openai/codex","version":"-","bin":{"codex":"cli"}}' > "$marked_package/package.json"
      make_vendor_binary "$marked_package/cli" codex 1.0.0
      direct_owner_write "$marked_package/cli"
      printf '# changed bytes\n' >> "$marked_package/cli"
      ln -s ../root/@openai/codex/cli "$HOME/foreign-npm/bin/codex"
      mkdir -p "$HOME/owned-bin"
      cat > "$HOME/owned-bin/codex" <<'EOF_MARKED_FOREIGN'
#!/bin/sh
printf '%s\n' "$*" >> "$HOME/marked-foreign-invocations"
printf 'codex 1.0.0\n'
EOF_MARKED_FOREIGN
      chmod +x "$HOME/owned-bin/codex"
      for marker_value in '' malformed; do
        printf '%s\n' "$marker_value" > "$HOME/owned-bin/codex.oh-my-setting-managed"
        marker_result=0
        doctor_command_digest_ok "$HOME/owned-bin/codex" --require-owner || marker_result=$?
        [ "$marker_result" = 1 ] || fail "invalid marker was treated as an unmarked copy"
      done
      printf 'sha256=%s\n' "$(sha256_file "$HOME/owned-bin/codex")" > "$HOME/owned-bin/codex.oh-my-setting-managed"
      mkdir -p "$HOME/owned-alias"
      ln -s "$HOME/owned-bin/codex" "$HOME/owned-alias/codex"
      export PATH="$PATH:$HOME/foreign-bin:$HOME/foreign-npm/bin:$HOME/owned-bin:$HOME/owned-alias"
      : > "$HOME/version-probes"
      doctor_provider_inventory > "$HOME/report"
      [ ! -e "$HOME/foreign-invocations" ] || fail "later inventory executed the unmarked foreign copy"
      [ ! -e "$HOME/marked-foreign-invocations" ] || fail "self-authored digest marker authorized foreign execution"
      grep -Fq "channel=npm version=unknown path=$malformed_package (not executed)" "$HOME/report" || fail "malformed metadata forged an inventory record"
      for path in "$HOME/owned-bin/codex" "$HOME/owned-alias/codex"; do
        grep -Fq "codex extra channel=unknown version=unknown path=$path (not executed) digest=matched" "$HOME/report" || fail "marked extra was not listed without execution"
      done
      [ "$(wc -l < "$HOME/version-probes" | tr -d ' ')" = 4 ] || fail "inventory did not limit probes to selected providers"
      grep -Fq "path=$HOME/repo/bin/codex (not executed) digest=unmarked" "$HOME/report" || fail "unmarked extra digest status missing"
      grep -Fq "path=$HOME/foreign-bin/claude (not executed) digest=invalid" "$HOME/report" || fail "invalid extra digest status missing"
      if [ -f "$HOME/path-report" ]; then
        grep -Fq '(not executed) digest=unknown' "$HOME/path-report" || fail "unsafe extra digest status missing"
      fi
      [ ! -s "$HOME/version-probes" ] || ! grep -Fxq "$HOME/foreign-bin/claude" "$HOME/version-probes" || fail "inventory executed an unverified marked copy"
      if grep -Fxq "$HOME/foreign-npm/bin/codex" "$HOME/version-probes"; then fail "inventory bypassed a digest marker on an npm target"; fi
      grep -Fq "channel=npm version=unverified path=$HOME/foreign-npm/bin/codex" "$HOME/report" || fail "marked npm target drift was omitted"
      grep -Fq "channel=unknown version=unverified path=$HOME/foreign-bin/claude" "$HOME/report" || fail "inventory omitted unverified-copy status"
      grep -Fq 'selected channel=vendor version=1.0.0' "$HOME/report" || fail "doctor selected vendor missing"
      grep -Fq 'channel=npm version=0.5.0' "$HOME/report" || fail "doctor nvm copy missing"
      grep -Fq "channel=npm version= path=$empty_package" "$HOME/report" || fail "empty inventory field shifted its package path"
      grep -Fq 'agy selected channel=oms-direct' "$HOME/report" || fail "doctor direct channel missing"
      for tool in claude codex pi; do
        grep -Fq "multiple copies of $tool" "$HOME/report" || fail "doctor $tool duplicate warning missing"
      done
      grep -Fq 'npm uninstall -g --prefix' "$HOME/report" || fail "doctor duplicate remedy missing"
      grep -Fq "rm -f -- $HOME/foreign-bin/claude" "$HOME/report" || fail "doctor foreign launcher cleanup missing"
      if grep -Fq '/versions/0.9.0' "$HOME/report"; then fail "doctor listed vendor-retained binary as a copy"; fi
      if grep -Fq "rm -f -- $HOME/.local/share/claude/versions/1.0.0" "$HOME/report"; then fail "doctor suggested removing the selected native copy"; fi
      check_vendor_provider_version claude npm.claude.version | grep -Fq 'vendor channel' || fail "doctor demanded snapshot equality"
      rm -rf "$HOME/.local" "$HOME/.nvm" "$HOME/.codex" "$HOME/.pi" "$HOME/alias-bin" "$HOME/foreign-bin" "$HOME/foreign-npm" "$HOME/repo" "$HOME/owned-bin" "$HOME/owned-alias"
      doctor_provider_inventory > "$HOME/report"
      [ ! -s "$HOME/report" ] || fail "doctor provider absence was noisy" ;;
    versions)
      for before in '' bad 1.0 01.0.0; do
        if provider_version_not_older "$before" 0.0.0; then fail "malformed version accepted"; fi
      done
      provider_version_not_older 0.0.0 0.0.0 || fail "zero/equal version refused"
      provider_version_not_older 1.0.0 1.0.0-rc.1 || fail "stable upgrade refused"
      if provider_version_not_older 1.0.0-rc.1 1.0.0; then fail "prerelease downgrade accepted"; fi ;;
    *) fail "unknown vendor case: $scenario" ;;
  esac
  [ ! -e "$HOME/unexpected" ] || fail "$scenario used npm/download unexpectedly"
)
# Snapshot resolution is independent of a caller's installed native provider.
provider_snapshot_case() (
  export HOME="$TMP/provider-home" PATH=/usr/bin:/bin
  export CODEX_HOME="$HOME/.codex"
  unset OH_MY_SETTING_TOOL_LOCK
  mkdir -p "$HOME"
  set --
  . "$ROOT/scripts/install-tools.sh"
  python3() {
    if [ "$1" = "$ROOT/scripts/lib/provider-latest.py" ]; then
      printf '%s\n' "$@" > "$TMP/provider-args"
      local output=""
      while [ "$#" -gt 0 ]; do
        if [ "$1" = --output ]; then output="$2"; break; fi
        shift
      done
      mkdir -p "$(dirname "$output")"
      cp "$LOCK" "$output"
    else
      command python3 "$@"
    fi
  }
  prepare_provider_versions node codex
  [ "$TOOL_LOCK" = "$HOME/.local/share/oh-my-setting/provider-tools.lock.json" ] ||
    fail "installer did not select resolved provider snapshot"
  grep -Fxq codex "$TMP/provider-args" || fail "selected provider was not resolved"
  if grep -Exq 'node|claude|agy' "$TMP/provider-args"; then fail "unselected provider was resolved"; fi
  python3() { return 23; }
  if prepare_provider_versions codex; then fail "release lookup failure was ignored"; fi
  OH_MY_SETTING_TOOL_LOCK="$LOCK" prepare_provider_versions codex ||
    fail "explicit lock should bypass release lookup"
)

vendor_cases='claude-native claude-pin claude-fresh claude-checksum claude-migration claude-local-npm claude-unowned pi agy codex-channel vendor-refresh doctor versions'
# External newer gh is not an OMS-owned drift; pins and owned bytes stay strict.
(
  mkdir -p "$TMP/gh-bin"
  printf '#!/usr/bin/env bash\nprintf "gh version %%s\\n" "$OMS_TEST_GH_VERSION"\n' > "$TMP/gh-bin/gh"
  chmod +x "$TMP/gh-bin/gh"
  export PATH="$TMP/gh-bin:$PATH"
  unset OH_MY_SETTING_TOOL_LOCK
  export REQUIRE_TOOLS=0
  export OMS_TEST_GH_VERSION=2.99.0
  # shellcheck source=scripts/lib/doctor-probe-memo.sh
  . "$ROOT/scripts/lib/doctor-probe-memo.sh"
  for function_name in command_has_locked_version external_gh_is_newer check_locked_direct_version doctor_sha256_file; do
    eval "$(sed -n "/^${function_name}()/,/^}/p" "$ROOT/scripts/doctor.sh")"
  done
  tool_lock_value() { printf '2.97.0\n'; }
  report_tool_drift() { printf 'drift: %s\n' "$*"; }
  check_locked_direct_version gh gh gh.version | grep '^note:.*newer' > /dev/null || fail "external newer gh should not require downgrade"
  for OMS_TEST_GH_VERSION in 2.9.0 2.99.0-rc1 invalid; do
    check_locked_direct_version gh gh gh.version | grep '^drift:' > /dev/null || fail "older or unverified gh escaped drift"
  done
  OMS_TEST_GH_VERSION=2.99.0
  REQUIRE_TOOLS=1
  check_locked_direct_version gh gh gh.version | grep '^drift:' > /dev/null || fail "strict gh pin was relaxed"
  REQUIRE_TOOLS=0
  OH_MY_SETTING_TOOL_LOCK="$LOCK"
  check_locked_direct_version gh gh gh.version | grep '^drift:' > /dev/null || fail "explicit gh pin was relaxed"
  unset OH_MY_SETTING_TOOL_LOCK
  touch "$TMP/gh-bin/gh.oh-my-setting-managed"
  check_locked_direct_version gh gh gh.version | grep '^drift:' > /dev/null || fail "owned gh version drift was ignored"
  OMS_TEST_GH_VERSION=2.97.0
  check_locked_direct_version gh gh gh.version | grep 'managed binary digest changed' > /dev/null || fail "owned gh integrity was ignored"
)

# Native Codex owns its standalone launcher; updates must not replace it with npm.
legacy_native_codex_case() (
  set --
  . "$ROOT/scripts/install-tools.sh"
  export CODEX_HOME="$TMP/codex-native"
  native_bin="$CODEX_HOME/packages/standalone/releases/fixture/bin"
  mkdir -p "$native_bin" "$TMP/native-path"
  cat > "$native_bin/codex" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  --version) echo "codex-cli $(cat "$CODEX_HOME/version")" ;;
  update) printf '%s\n' "$NATIVE_EXPECTED" > "$CODEX_HOME/version"; touch "$CODEX_HOME/updated" ;;
  *) exit 1 ;;
esac
EOF
  chmod +x "$native_bin/codex"
  ln -s "$native_bin/codex" "$TMP/native-path/codex"
  export PATH="$TMP/native-path:$PATH"
  export NATIVE_EXPECTED
  NATIVE_EXPECTED="$(tool_lock_get npm.codex.version)"
  printf '0.0.1\n' > "$CODEX_HOME/version"
  standalone_codex || fail "standalone Codex was not detected"
  export OH_MY_SETTING_TOOL_LOCK="$LOCK"
  if refresh_standalone_codex > /dev/null 2>&1; then fail "native updater ignored explicit pin"; fi
  [ ! -e "$CODEX_HOME/updated" ] || fail "explicit pin triggered native update"
  unset OH_MY_SETTING_TOOL_LOCK
  refresh_standalone_codex >/dev/null || fail "native Codex refresh failed"
  preflight_npm_shim codex || fail "native launcher rejected by npm preflight"
  install_npm_global codex >/dev/null || fail "current native Codex was not retained"
  write_npm_shim codex || fail "native launcher not preserved"
  verify_resolved_npm_binary codex || fail "native version verification failed"
  [ -L "$TMP/native-path/codex" ] || fail "native launcher was replaced"
  for function_name in normalize_doctor_npm_path doctor_command_digest_ok doctor_sha256_file doctor_provider_inventory check_vendor_provider_version check_locked_npm_version; do
    eval "$(sed -n "/^${function_name}()/,/^}/p" "$ROOT/scripts/doctor.sh")"
  done
  . "$ROOT/scripts/lib/doctor-probe-memo.sh"
  eval "$(sed -n '/^provider_lock_path()/,/^}/p' "$ROOT/scripts/doctor.sh")"
  export HOME="$TMP/native-doctor-home"
  export NVM_DIR="$HOME/.nvm"
  node_bin="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
  mkdir -p "$HOME/.local/bin" "$node_bin"
  ln -s "$native_bin/codex" "$HOME/.local/bin/codex"
  printf '#!/usr/bin/env bash\necho v%s\n' "$NODE_VERSION" > "$node_bin/node"
  printf '#!/usr/bin/env bash\necho stale-codex\n' > "$node_bin/codex"
  chmod +x "$node_bin/node" "$node_bin/codex"
  # shellcheck disable=SC2034 # Consumed by the doctor function loaded above.
  INSTALL_ROOT="$ROOT"
  eval "$(sed -n '/^load_user_tool_paths()/,/^}/p' "$ROOT/scripts/doctor.sh")"
  load_user_tool_paths
  [ "$(command -v codex)" = "$HOME/.local/bin/codex" ] ||
    fail "doctor shadows the active native Codex with a stale npm executable"
  printf 'nvm() { export PATH="%s:$PATH"; }\n' "$node_bin" > "$NVM_DIR/nvm.sh"
  eval "$(sed -n '/^load_user_tool_paths()/,/^}/p' "$ROOT/scripts/status.sh")"
  # shellcheck disable=SC2034 # Read by the extracted status function.
  VERBOSE=1
  load_user_tool_paths
  [ "$(command -v codex)" = "$HOME/.local/bin/codex" ] ||
    fail "verbose status shadows the active native Codex"
  # Peer calls resolve the nvm default once and reuse the bin dir until an
  # input of that resolution (alias files, installed versions) changes.
  printf 'echo x >> "$NVM_DIR/sourced"\nnvm() { export NVM_BIN="%s" PATH="%s:$PATH"; }\n' \
    "$node_bin" "$node_bin" > "$NVM_DIR/nvm.sh"
  mkdir -p "$NVM_DIR/alias"
  printf 'lts/*\n' > "$NVM_DIR/alias/default"
  peer_node() {
    env -u OMS_TEST_PROVIDER_PATH_ONLY bash -c '. "$1/scripts/lib/peer-common.sh"; load_user_tool_paths; command -v node' _ "$ROOT"
  }
  [ "$(peer_node)" = "$node_bin/node" ] || fail "peer tool paths missed the nvm default"
  [ "$(peer_node)" = "$node_bin/node" ] || fail "cached peer tool paths missed the nvm default"
  [ "$(wc -l < "$NVM_DIR/sourced" | tr -d ' ')" = 1 ] || fail "an unchanged nvm default was resolved twice"
  printf 'node\n' > "$NVM_DIR/alias/default"
  peer_node >/dev/null
  [ "$(wc -l < "$NVM_DIR/sourced" | tr -d ' ')" = 2 ] || fail "a changed nvm alias reused a stale resolution"
  printf 'work\n' > "$NVM_DIR/alias/default"
  printf 'v22\n' > "$NVM_DIR/alias/work"
  peer_node >/dev/null
  printf 'v24\n' > "$NVM_DIR/alias/work"
  peer_node >/dev/null
  [ "$(wc -l < "$NVM_DIR/sourced" | tr -d ' ')" = 4 ] ||
    fail "a changed alias that default names reused a stale resolution"
  [ "$(provider_lock_path)" = "$LOCK" ] || fail "doctor lost bootstrap fallback"
  snapshot="$HOME/.local/share/oh-my-setting/provider-tools.lock.json"
  mkdir -p "$(dirname "$snapshot")"
  cp "$LOCK" "$snapshot"
  [ "$(provider_lock_path)" = "$snapshot" ] || fail "doctor ignored provider snapshot"
  [ "$(OH_MY_SETTING_TOOL_LOCK="$LOCK" provider_lock_path)" = "$LOCK" ] ||
    fail "doctor ignored explicit pin over snapshot"
  tool_lock_value() { tool_lock_get "$1"; }
  command_has_locked_version() { command_has_version "$@"; }
  report_tool_drift() { printf 'native-drift: %s\n' "$*"; }
  check_locked_npm_version codex | grep 'vendor channel' > /dev/null || fail "doctor did not recognize native Codex"
  printf '0.0.1\n' > "$CODEX_HOME/version"
  check_locked_npm_version codex | grep 'vendor channel' > /dev/null || fail "doctor demanded snapshot equality"
  export OH_MY_SETTING_TOOL_LOCK="$LOCK"
  check_locked_npm_version codex | grep 'native-drift:' > /dev/null || fail "doctor accepted explicit native pin drift"
  unset OH_MY_SETTING_TOOL_LOCK
  printf '0.0.3\n' > "$CODEX_HOME/version"
  export NATIVE_EXPECTED=0.0.2
  if refresh_standalone_codex >/dev/null 2>&1; then fail "native downgrade passed"; fi
  export CODEX_HOME="$TMP/foreign-codex"
  if standalone_codex; then fail "foreign path was accepted as standalone"; fi
)

if [ "${1:-}" = --provider-snapshot-case ]; then
  provider_snapshot_case
  echo "tool-lock-smoke provider snapshot: ok"
  exit 0
fi
if [ "${1:-}" = --native-codex-case ]; then
  legacy_native_codex_case
  echo "tool-lock-smoke native codex: ok"
  exit 0
fi
if [ "${1:-}" = --vendor-case ]; then
  provider_vendor_case "${2:?vendor case required}"
  echo "tool-lock-smoke vendor $2: ok"
  exit 0
fi
for vendor_case in $vendor_cases; do provider_vendor_case "$vendor_case"; done

legacy_native_codex_case

# Integrity verification binds the package bytes that npm will install, not
# only registry metadata fetched in an earlier request.
printf 'locked package bytes\n' > "$TMP/package.tgz"
package_integrity="$(python3 - "$TMP/package.tgz" <<'PY'
import base64, hashlib, sys
print("sha512-" + base64.b64encode(hashlib.sha512(open(sys.argv[1], "rb").read()).digest()).decode("ascii"))
PY
)"
python3 "$HELPER" --lock "$LOCK" verify-sri \
  --integrity "$package_integrity" --file "$TMP/package.tgz" >/dev/null ||
  fail "matching npm package integrity was rejected"
printf 'changed\n' >> "$TMP/package.tgz"
if python3 "$HELPER" --lock "$LOCK" verify-sri \
    --integrity "$package_integrity" --file "$TMP/package.tgz" >/dev/null 2>&1; then
  fail "changed npm package bytes passed locked integrity"
fi

# A digest-authenticated archive is still data: extraction may not write
# outside its dedicated staging directory through member names.
python3 - "$TMP/traversal.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"outside\n"
    item = tarfile.TarInfo("../outside")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
PY
mkdir -p "$TMP/extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/traversal.tar.gz" --dest "$TMP/extracted" >/dev/null 2>&1; then
  fail "archive traversal member was accepted"
fi
[ ! -e "$TMP/outside" ] || fail "archive extraction escaped its staging directory"

python3 - "$TMP/safe.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("node", "node/bin", "node/lib", "node/lib/npm"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        item.mode = 0o755
        archive.addfile(item)
    data = b"npm\n"
    item = tarfile.TarInfo("node/lib/npm/cli.js")
    item.size = len(data)
    item.mode = 0o644
    archive.addfile(item, io.BytesIO(data))
    link = tarfile.TarInfo("node/bin/npm")
    link.type = tarfile.SYMTYPE
    link.linkname = "../lib/npm/cli.js"
    archive.addfile(link)
PY
mkdir -p "$TMP/safe-extracted"
python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
  --source "$TMP/safe.tar.gz" --dest "$TMP/safe-extracted" >/dev/null ||
  fail "contained archive symlink was rejected"
[ "$(cat "$TMP/safe-extracted/node/bin/npm")" = npm ] ||
  fail "safe archive did not extract correctly"

# Windows treats C:payload as a drive-relative path even though it is not
# absolute. Both member names and link targets must reject that spelling
# before extraction on every host, so Linux CI protects native Git Bash too.
python3 - "$TMP/drive-relative.tar.gz" "$TMP/drive-link.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"drive\n"
    item = tarfile.TarInfo("C:payload")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
with tarfile.open(sys.argv[2], "w:gz") as archive:
    item = tarfile.TarInfo("safe/link")
    item.type = tarfile.SYMTYPE
    item.linkname = "C:payload"
    archive.addfile(item)
PY
for unsafe in drive-relative drive-link; do
  mkdir -p "$TMP/$unsafe-extracted"
  if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
      --source "$TMP/$unsafe.tar.gz" --dest "$TMP/$unsafe-extracted" \
      >/dev/null 2>&1; then
    fail "Windows drive-relative $unsafe archive path was accepted"
  fi
done

# A sequence of individually contained links must not redirect a later file
# write outside staging. Regular files are extracted without following archive
# links, then the complete link graph is checked before use.
python3 - "$TMP/link-chain.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("p", "x"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        archive.addfile(item)
    first = tarfile.TarInfo("p/a")
    first.type = tarfile.SYMTYPE
    first.linkname = "../x"
    archive.addfile(first)
    second = tarfile.TarInfo("p/a/b")
    second.type = tarfile.SYMTYPE
    second.linkname = "../../outside"
    archive.addfile(second)
    data = b"escaped\n"
    payload = tarfile.TarInfo("p/a/b/payload")
    payload.size = len(data)
    archive.addfile(payload, io.BytesIO(data))
PY
mkdir -p "$TMP/chain-root/stage"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/link-chain.tar.gz" --dest "$TMP/chain-root/stage" \
    >/dev/null 2>&1; then
  fail "archive symlink-chain pivot was accepted"
fi
[ ! -e "$TMP/chain-root/outside/payload" ] ||
  fail "archive symlink chain wrote outside staging before rejection"

python3 - "$TMP/ads.tar.gz" <<'PY'
import io, tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    data = b"ads\n"
    item = tarfile.TarInfo("safe/payload:stream")
    item.size = len(data)
    archive.addfile(item, io.BytesIO(data))
PY
mkdir -p "$TMP/ads-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/ads.tar.gz" --dest "$TMP/ads-extracted" >/dev/null 2>&1; then
  fail "Windows alternate-data-stream archive path was accepted"
fi

# Device, FIFO, and unknown tar entry types are never required by the locked
# tool payloads. Reject them before extraction instead of relying on each
# tarfile implementation's handling of special entries.
python3 - "$TMP/special.tar.gz" <<'PY'
import tarfile, sys
with tarfile.open(sys.argv[1], "w:gz") as archive:
    item = tarfile.TarInfo("device")
    item.type = tarfile.CHRTYPE
    archive.addfile(item)
PY
mkdir -p "$TMP/special-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive tar.gz \
    --source "$TMP/special.tar.gz" --dest "$TMP/special-extracted" \
    >/dev/null 2>&1; then
  fail "archive special file was accepted"
fi

python3 - "$TMP/duplicate.zip" <<'PY'
import warnings, zipfile, sys
with warnings.catch_warnings():
    warnings.simplefilter("ignore")
    with zipfile.ZipFile(sys.argv[1], "w") as archive:
        archive.writestr("payload/file", b"first")
        archive.writestr("payload/file", b"second")
PY
mkdir -p "$TMP/duplicate-zip-extracted"
if python3 "$HELPER" --lock "$LOCK" extract --archive zip \
    --source "$TMP/duplicate.zip" --dest "$TMP/duplicate-zip-extracted" \
    >/dev/null 2>&1; then
  fail "zip archive with a duplicate member path was accepted"
fi

# Native npm packages are installed only after their embedded identity is
# checked, and the helper leaves a complete directory rather than a tarball or
# an interrupted backup.
python3 - "$TMP/native.tgz" <<'PY'
import io, json, tarfile, sys
manifest = json.dumps({"name": "@fixture/native", "version": "1.2.3"}).encode()
with tarfile.open(sys.argv[1], "w:gz") as archive:
    for name in ("package", "package/bin"):
        item = tarfile.TarInfo(name)
        item.type = tarfile.DIRTYPE
        item.mode = 0o755
        archive.addfile(item)
    item = tarfile.TarInfo("package/package.json")
    item.size = len(manifest)
    archive.addfile(item, io.BytesIO(manifest))
    binary = b"native\n"
    item = tarfile.TarInfo("package/bin/native")
    item.size = len(binary)
    item.mode = 0o755
    archive.addfile(item, io.BytesIO(binary))
PY
mkdir -p "$TMP/native-root"
python3 "$HELPER" --lock "$LOCK" install-npm-payload \
  --file "$TMP/native.tgz" --dest "$TMP/native-root/@fixture/native" \
  --root "$TMP/native-root" \
  --name '@fixture/native' --version 1.2.3 >/dev/null ||
  fail "verified native npm payload was not installed"
python3 "$HELPER" --lock "$LOCK" verify-installed-npm \
  --path "$TMP/native-root/@fixture/native" --name '@fixture/native' \
  --version 1.2.3 >/dev/null || fail "installed native npm payload lost its identity"
[ ! -e "$TMP/native-root/@fixture/native.oh-my-setting-backup" ] ||
  fail "native npm install leaked its recovery backup"
if python3 "$HELPER" --lock "$LOCK" verify-npm-package \
    --file "$TMP/native.tgz" --name '@fixture/wrong' --version 1.2.3 \
    >/dev/null 2>&1; then
  fail "native npm package with a mismatched identity was accepted"
fi

# Direct CLI payloads use a same-directory stage and recoverable swap. They
# refuse an unowned live target, restore an interrupted pre-rename update, and
# finish an interrupted post-rename update without leaving backup files.
(
  export HOME="$TMP/direct-home"
  mkdir -p "$HOME/.local/bin"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  make_direct_fixture() {
    printf '#!/usr/bin/env bash\necho "fixture %s"\n' "$2" > "$1"
    chmod +x "$1"
  }
  source_file="$TMP/direct-source"
  target="$HOME/.local/bin/fixture"
  make_direct_fixture "$source_file" 2.0.0
  make_direct_fixture "$target" 1.0.0
  digest="$(sha256_file "$source_file")"
  if install_direct_binary "$source_file" "$target" sha256 "$digest" \
      2.0.0 fixture >/dev/null 2>&1; then
    fail "direct tool installer overwrote an unowned live target"
  fi
  [ "$($target --version 2>/dev/null || $target)" = 'fixture 1.0.0' ] ||
    fail "direct tool collision changed the user-owned target"
  printf 'sha256=%s\nextra' "$(sha256_file "$target")" > \
    "$target.oh-my-setting-managed"
  if install_direct_binary "$source_file" "$target" sha256 "$digest" \
      2.0.0 fixture >/dev/null 2>&1; then
    fail "malformed ownership sidecar authorized a direct tool overwrite"
  fi
  rm -f "$target.oh-my-setting-managed"

  rm -f "$target"
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    2.0.0 fixture >/dev/null || fail "direct tool first install failed"
  [ -f "$target.oh-my-setting-managed" ] ||
    fail "direct tool install did not record ownership"

  make_direct_fixture "$source_file" 3.0.0
  digest="$(sha256_file "$source_file")"
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    3.0.0 fixture >/dev/null || fail "owned direct tool update failed"

  printf '# same-version tamper\n' >> "$target"
  if direct_locked_command_is_current fixture "$target" 3.0.0; then
    fail "managed direct tool tamper was accepted by the fast path"
  fi
  install_direct_binary "$source_file" "$target" sha256 "$digest" \
    3.0.0 fixture >/dev/null || fail "managed direct tool tamper was not repaired"
  [ "$(sha256_file "$target")" = "$digest" ] ||
    fail "managed direct tool repair did not restore the locked payload"

  make_direct_fixture "$source_file" 4.0.0
  digest="$(sha256_file "$source_file")"
  cp "$source_file" "$target.oh-my-setting-stage"
  mv "$target" "$target.oh-my-setting-backup"
  recover_direct_install "$target" sha256 "$digest" 4.0.0 >/dev/null ||
    fail "interrupted direct tool update was not recovered"
  [ "$($target)" = 'fixture 3.0.0' ] &&
    [ ! -e "$target.oh-my-setting-stage" ] &&
    [ ! -e "$target.oh-my-setting-backup" ] ||
    fail "pre-rename direct tool recovery did not restore the previous target"

  mv "$target" "$target.oh-my-setting-backup"
  cp "$source_file" "$target"
  chmod +x "$target"
  recover_direct_install "$target" sha256 "$digest" 4.0.0 >/dev/null ||
    fail "post-rename direct tool update was not completed"
  [ "$($target)" = 'fixture 4.0.0' ] &&
    [ ! -e "$target.oh-my-setting-backup" ] ||
    fail "post-rename direct tool recovery left stale state"

  # uvx is a launcher that discovers an adjacent uv binary. Its staged name
  # therefore cannot be verified unless the matching uv stage is present, and
  # both collision checks must finish before either live command changes.
  pair_source="$TMP/uv-pair/source"
  pair_bin="$TMP/uv-pair/bin"
  mkdir -p "$pair_source" "$pair_bin"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "uv 5.0.0"' > "$pair_source/uv"
  cat > "$pair_source/uvx" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd -P)"
case "$(basename "$0")" in
  uvx.oh-my-setting-stage) companion="$here/uv.oh-my-setting-stage" ;;
  *) companion="$here/uv" ;;
esac
[ -x "$companion" ] || { echo "missing adjacent uv" >&2; exit 1; }
exec "$companion" --version
EOF
  chmod +x "$pair_source/uv" "$pair_source/uvx"
  uv_digest="$(sha256_file "$pair_source/uv")"
  uvx_digest="$(sha256_file "$pair_source/uvx")"
  printf '%s\n' '#!/usr/bin/env bash' 'echo "user uvx 5.0.0"' > "$pair_bin/uvx"
  chmod +x "$pair_bin/uvx"
  if install_uv_binaries "$pair_source/uv" "$pair_source/uvx" \
      "$pair_bin/uv" "$pair_bin/uvx" "$uv_digest" "$uvx_digest" \
      5.0.0 fixture >/dev/null 2>&1; then
    fail "uv pair installer overwrote an unowned uvx collision"
  fi
  [ ! -e "$pair_bin/uv" ] ||
    fail "uv pair installer changed uv before preflighting uvx"
  rm -f "$pair_bin/uvx"
  install_uv_binaries "$pair_source/uv" "$pair_source/uvx" \
    "$pair_bin/uv" "$pair_bin/uvx" "$uv_digest" "$uvx_digest" \
    5.0.0 fixture >/dev/null || fail "adjacent uv/uvx pair install failed"
  [ "$($pair_bin/uv --version)" = 'uv 5.0.0' ] &&
    [ "$($pair_bin/uvx --version)" = 'uv 5.0.0' ] &&
    [ -f "$pair_bin/uv.oh-my-setting-managed" ] &&
    [ -f "$pair_bin/uvx.oh-my-setting-managed" ] ||
    fail "uv pair install did not publish verified managed commands"

  npm_root="$TMP/npm-transaction/root"
  npm_bin="$TMP/npm-transaction/bin"
  npm_package="$npm_root/@fixture/tool"
  mkdir -p "$npm_package" "$npm_bin"
  printf 'old package\n' > "$npm_package/state"
  ln -s ../root/@fixture/tool/state "$npm_bin/fixture"
  npm_transaction_begin "$npm_package" "$npm_bin" fixture ||
    fail "npm rollback transaction could not start"
  mkdir -p "$npm_package"
  printf 'partial package\n' > "$npm_package/state"
  printf 'partial binary\n' > "$npm_bin/fixture"
  npm_transaction_restore "$npm_package" "$npm_bin" fixture ||
    fail "npm rollback transaction could not restore"
  [ "$(cat "$npm_package/state")" = 'old package' ] &&
    [ -L "$npm_bin/fixture" ] &&
    [ ! -e "$npm_package.oh-my-setting-transaction" ] &&
    [ ! -e "$npm_package.oh-my-setting-backup" ] ||
    fail "failed npm install did not restore its previous package and shim"

  # A verified install must survive an interruption during backup cleanup.
  npm_transaction_begin "$npm_package" "$npm_bin" fixture || fail "commit fixture could not begin"
  mkdir -p "$npm_package"
  printf 'new package\n' > "$npm_package/state"
  ln -s ../root/@fixture/tool/state "$npm_bin/fixture"
  rm() {
    case "$*" in
      *"$npm_package.oh-my-setting-backup"*)
        command rm -f "$npm_package.oh-my-setting-backup/state"
        return 1 ;;
    esac
    command rm "$@"
  }
  if npm_transaction_commit "$npm_package" "$npm_bin" fixture; then fail "commit cleanup failure was ignored"; fi
  unset -f rm
  npm_package_is_current() { return 1; }
  npm_transaction_recover fixture "$npm_package" "$npm_bin" fixture || fail "committed transaction could not recover"
  [ "$(cat "$npm_package/state")" = 'new package' ] && [ -L "$npm_bin/fixture" ] &&
    [ ! -e "$npm_package.oh-my-setting-backup" ] || fail "interrupted commit lost the new package or left backup"
  npm_transaction_begin "$npm_package" "$npm_bin" fixture fixture || fail "optional commit fixture could not begin"
  mkdir -p "$npm_package"
  printf 'newer package\n' > "$npm_package/state"
  ln -s ../root/@fixture/tool/state "$npm_bin/fixture"
  rm() {
    case "$*" in *"$npm_package.oh-my-setting-backup"*) return 1 ;; esac
    command rm "$@"
  }
  if npm_transaction_commit "$npm_package" "$npm_bin" fixture; then fail "optional commit cleanup failure was ignored"; fi
  unset -f rm
  optional_npm_recover "$npm_root" "$npm_bin" || fail "optional committed cleanup did not recover"
  [ "$(cat "$npm_package/state")" = 'newer package' ] && [ -L "$npm_bin/fixture" ] &&
    [ ! -e "$npm_package.oh-my-setting-backup" ] || fail "optional committed recovery rolled back"

  # Old markers have no absent sentinels; a new dangling link belongs to the failed install.
  legacy="$npm_root/legacy"
  mkdir -p "$legacy" "$npm_bin/.legacy.oh-my-setting-backup"
  printf 'schema=1\nphase=backed_up\nhad_package=0\nbinary=legacy\n' > "$legacy.oh-my-setting-transaction"
  printf 'partial\n' > "$legacy/cli"
  ln -s ../root/legacy/cli "$npm_bin/legacy"
  ln -s ../unrelated "$npm_bin/legacy.cmd"
  npm_transaction_restore "$legacy" "$npm_bin" legacy || fail "legacy transaction could not restore"
  [ ! -e "$npm_bin/legacy" ] && [ ! -L "$npm_bin/legacy" ] || fail "legacy rollback left dangling shim"
  [ -L "$npm_bin/legacy.cmd" ] || fail "legacy rollback removed an unrelated link"
  mkdir -p "$legacy" "$npm_bin/.legacy.oh-my-setting-backup"
  printf 'schema=1\nphase=backed_up\nhad_package=1\nbinary=legacy\n' > "$legacy.oh-my-setting-transaction"
  printf 'restored\n' > "$legacy/cli"
  ln -s ../root/legacy/cli "$npm_bin/legacy"
  npm_transaction_restore "$legacy" "$npm_bin" legacy || fail "resumed legacy restoration failed"
  [ -L "$npm_bin/legacy" ] || fail "resumed legacy restoration removed a recovered link"
)

# Optional provider refresh uses package metadata, not provider execution.
(
  export HOME="$TMP/optional-home"
  mkdir -p "$HOME" "$TMP/optional-bin" "$TMP/optional-root/cursor-pkg"
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli"}}' > "$TMP/optional-root/cursor-pkg/package.json"
  printf '#!/bin/sh\nexit 99\n' > "$TMP/optional-root/cursor-pkg/cli"
  chmod +x "$TMP/optional-root/cursor-pkg/cli"
  ln -s "$TMP/optional-root/cursor-pkg/cli" "$TMP/optional-bin/cursor-agent"
  export PATH="$TMP/optional-bin:$PATH"
  set --
  . "$ROOT/scripts/install-tools.sh"
  # A binary inside a package is not necessarily its declared CLI.
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"other"}}' > "$TMP/optional-root/cursor-pkg/package.json"
  if optional_npm_identity "$TMP/optional-root" "$TMP/optional-bin/cursor-agent" cursor-agent >/dev/null; then fail "undeclared package executable accepted"; fi
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli"}}' > "$TMP/optional-root/cursor-pkg/package.json"
  for bad_name in -bad .bad _bad; do
    mkdir -p "$TMP/optional-root/$bad_name"
    printf '{"name":"%s","version":"1.0.0","bin":{"cursor-agent":"cli"}}\n' "$bad_name" > "$TMP/optional-root/$bad_name/package.json"
    touch "$TMP/optional-root/$bad_name/cli"
    if optional_npm_identity "$TMP/optional-root" "$TMP/optional-root/$bad_name/cli" cursor-agent >/dev/null; then fail "invalid npm name accepted"; fi
  done
  npm_global_root() { printf '%s\n' "$TMP/optional-root"; }
  npm_global_prefix() { printf '%s\n' "$TMP/optional-root"; }
  npm_global_bin_dir() { printf '%s\n' "$TMP/optional-bin"; }
  npm() {
    case "$1" in
      view)
        case "$*" in *versions) printf '%s\n' '["1.0.0","2.0.0","3.0.0"]' ;; *) printf '%s\n' '{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2020-01-02T00:00:00Z","3.0.0":"2999-01-01T00:00:00Z"}' ;; esac
        ;;
      install)
        printf '%s\n' "$*" >> "$TMP/optional-log"
        case "$*" in *cursor-pkg@2.0.0)
          if [ "${optional_fail:-0}" = 1 ]; then
            rm -rf "$TMP/optional-root/cursor-pkg"
            rm -f "$TMP/optional-bin/cursor-agent" "$TMP/optional-bin/cursor-extra" \
              "$TMP/optional-bin/cursor-extra.cmd" "$TMP/optional-bin/cursor-extra.ps1"
            mkdir -p "$TMP/optional-root/cursor-pkg"
            return 1
          fi ;;
        esac
        case "$*" in *cursor-pkg@1.0.0) version=1.0.0 ;; *) version=2.0.0 ;; esac
        mkdir -p "$TMP/optional-root/cursor-pkg"
        printf '#!/bin/sh\nexit 99\n' > "$TMP/optional-root/cursor-pkg/cli"
        chmod +x "$TMP/optional-root/cursor-pkg/cli"
        ln -sf "$TMP/optional-root/cursor-pkg/cli" "$TMP/optional-bin/cursor-agent"
        printf '{"name":"cursor-pkg","version":"%s","bin":{"cursor-agent":"cli"}}\n' "$version" > "$TMP/optional-root/cursor-pkg/package.json"
        ;;
      config) printf '%s\n' "$*" >> "$TMP/optional-config"; return 1 ;;
      *) return 1 ;;
    esac
  }
  if [ "$(id -u)" != 0 ]; then
  mkdir -p "$TMP/readonly-prefix"
  chmod a-w "$TMP/readonly-prefix"
  npm_global_prefix() { printf '%s\n' "$TMP/readonly-prefix"; }
  refresh_optional_providers > "$TMP/optional-out" 2>&1 || fail "unwritable prefix failed"
  grep -Fq 'not writable' "$TMP/optional-out" || fail "unwritable prefix not reported"
  [ ! -e "$TMP/optional-log" ] && [ ! -e "$TMP/optional-config" ] || fail "unwritable prefix changed npm state"
  chmod u+w "$TMP/readonly-prefix"
  fi
  npm_global_prefix() { printf '%s\n' "$TMP/optional-root"; }
  refresh_optional_providers > "$TMP/optional-out" 2>&1 || fail "optional refresh failed"
  grep -Fq 'cursor-pkg@2.0.0' "$TMP/optional-log" || fail "optional stable release not refreshed"
  grep -Fq -- '--before=' "$TMP/optional-log" || fail "dependency age floor missing"
  : > "$TMP/optional-log"
  refresh_optional_providers > "$TMP/optional-out" 2>&1 || fail "current optional refresh failed"
  [ ! -s "$TMP/optional-log" ] || fail "too-new release was installed"
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli"}}' > "$TMP/optional-root/cursor-pkg/package.json"
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli","cursor-extra":"cli"}}' > "$TMP/optional-root/cursor-pkg/package.json"
  printf 'original\n' > "$TMP/optional-root/cursor-pkg/retained"
  ln -s "$TMP/optional-root/cursor-pkg/cli" "$TMP/optional-bin/cursor-extra"
  printf 'old cmd\n' > "$TMP/optional-bin/cursor-extra.cmd"
  printf 'old ps1\n' > "$TMP/optional-bin/cursor-extra.ps1"
  optional_fail=1
  refresh_optional_providers > "$TMP/optional-out" 2>&1 || fail "optional failure escaped"
  grep -Fq 'warning: cursor:' "$TMP/optional-out" || fail "optional failure did not warn"
  [ "$(cat "$TMP/optional-root/cursor-pkg/retained")" = original ] || fail "optional package backup was not restored"
  [ -L "$TMP/optional-bin/cursor-agent" ] && [ -L "$TMP/optional-bin/cursor-extra" ] || fail "optional bin links were not restored"
  [ "$(cat "$TMP/optional-bin/cursor-extra.cmd")" = 'old cmd' ] &&
    [ "$(cat "$TMP/optional-bin/cursor-extra.ps1")" = 'old ps1' ] || fail "optional Windows shims were not restored"
  if grep -Fq 'cursor-pkg@1.0.0' "$TMP/optional-log"; then fail "optional restoration used npm"; fi
  npm_transaction_restore() { return 1; }
  if refresh_optional_providers > "$TMP/optional-out" 2>&1; then fail "failed restoration returned success"; fi
  grep -Fq 'restoration failed' "$TMP/optional-out" || fail "failed restoration did not warn"
  unset -f npm_transaction_restore
  eval "$(sed -n '/^npm_transaction_restore()/,/^}/p' "$ROOT/scripts/install-tools.sh")"
  npm_transaction_restore "$TMP/optional-root/cursor-pkg" "$TMP/optional-bin" cursor-agent || fail "fixture recovery failed"
  optional_fail=0
  npm_transaction_begin "$TMP/optional-root/cursor-pkg" "$TMP/optional-bin" cursor-agent cursor-agent cursor-extra || fail "interruption fixture could not back up"
  optional_npm_recover "$TMP/optional-root" "$TMP/optional-bin" || fail "interrupted backup did not recover"
  [ -L "$TMP/optional-bin/cursor-agent" ] && [ -L "$TMP/optional-bin/cursor-extra" ] || fail "interrupted backup lost links"
  : > "$TMP/optional-log"
  OH_MY_SETTING_TOOL_LOCK="$LOCK" refresh_optional_providers > "$TMP/optional-out" 2>&1
  [ ! -s "$TMP/optional-log" ] || fail "explicit lock refreshed optional provider"
  rm "$TMP/optional-bin/cursor-agent"
  printf '#!/bin/sh\nexit 99\n' > "$TMP/optional-bin/cursor-agent"
  chmod +x "$TMP/optional-bin/cursor-agent"
  refresh_optional_providers > "$TMP/optional-out" 2>&1
  grep -Fq 'cursor: skipped' "$TMP/optional-out" || fail "non-npm provider was not skipped"
  [ ! -s "$TMP/optional-log" ] || fail "non-npm provider was installed"
  # A Windows npm shim lives in the prefix, while its target is in node_modules.
  mkdir -p "$TMP/win-prefix/node_modules/cursor-pkg"
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli.js"}}' > "$TMP/win-prefix/node_modules/cursor-pkg/package.json"
  printf '%s\n' '#!/bin/sh' 'exec node "$basedir/node_modules/cursor-pkg/cli.js" "$@"' > "$TMP/win-prefix/cursor-agent"
  chmod +x "$TMP/win-prefix/cursor-agent"
  touch "$TMP/win-prefix/node_modules/cursor-pkg/cli.js"
  oms_platform_is_windows() { return 0; }
  npm_global_prefix() { printf '%s\n' "$TMP/win-prefix"; }
  identity="$(optional_npm_identity "$TMP/win-prefix/node_modules" "$TMP/win-prefix/cursor-agent" cursor-agent)" || fail "Windows npm shim not recognized"
  [ "$identity" = "$(printf 'cursor-pkg\t1.0.0')" ] || fail "Windows shim identity incorrect"
  printf '%s\n' '@ECHO off' 'endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%" "%dp0%\node_modules\cursor-pkg\cli.js" %*' > "$TMP/win-prefix/cursor-agent.cmd"
  optional_npm_identity "$TMP/win-prefix/node_modules" "$TMP/win-prefix/cursor-agent.cmd" cursor-agent >/dev/null || fail "Windows cmd shim not recognized"
  printf '%s\n' '#!/bin/sh' '# exec node "$basedir/node_modules/cursor-pkg/cli.js"' 'exit 0' > "$TMP/win-prefix/cursor-agent"
  if optional_npm_identity "$TMP/win-prefix/node_modules" "$TMP/win-prefix/cursor-agent" cursor-agent >/dev/null; then fail "comment-only Windows shim accepted"; fi
  printf '%s\n' '#!/bin/sh' 'exit 0' > "$TMP/win-prefix/cursor-agent"
  if optional_npm_identity "$TMP/win-prefix/node_modules" "$TMP/win-prefix/cursor-agent" cursor-agent >/dev/null; then fail "unrelated Windows shim accepted"; fi
  npm() { case "$*" in *versions) printf '%s\n' '[]' ;; *) printf '%s\n' '{}' ;; esac; }
  [ -z "$(optional_npm_target fixture 0.0.0)" ] || fail "empty release list selected a version"
  npm() { case "$*" in *versions) printf '%s\n' '["bad","0.0.0"]' ;; *) printf '%s\n' '{"bad":"2020-01-01T00:00:00Z","0.0.0":"2020-01-01T00:00:00Z"}' ;; esac; }
  [ -z "$(optional_npm_target fixture 0.0.0)" ] || fail "malformed or equal version selected"
  npm() { case "$*" in *versions) printf '%s\n' '["1.0.0","1.0.0-rc.1"]' ;; *) printf '%s\n' '{"1.0.0":"2020-01-01T00:00:00Z","1.0.0-rc.1":"2020-01-01T00:00:00Z"}' ;; esac; }
  [ "$(optional_npm_target fixture 1.0.0-rc.1)" = 1.0.0 ] || fail "stable release did not supersede prerelease"
  npm() { case "$*" in *versions) printf '%s\n' '["1.0.1+build"]' ;; *) printf '%s\n' '{"1.0.1+build":"2020-01-01T00:00:00Z"}' ;; esac; }
  [ "$(optional_npm_target fixture 1.0.0)" = 1.0.1+build ] || fail "stable build metadata was excluded"
  npm() { case "$*" in *versions) printf '%s\n' '["1.0.0"]' ;; *) printf '%s\n' '{"1.0.0":"2020-01-01T00:00:00Z","2.0.0":"2020-01-01T00:00:00Z"}' ;; esac; }
  [ -z "$(optional_npm_target fixture 1.0.0)" ] || fail "unpublished release selected"
  npm() { printf '%s\n' 'invalid'; }
  if optional_npm_target fixture 0.0.0 >/dev/null; then fail "malformed release metadata accepted"; fi
)

# Exercise the real --providers entry point with only an optional provider.
(
  home="$TMP/optional-only-home"
  bin="$TMP/optional-only-bin"
  prefix="$TMP/optional-only-prefix"
  mkdir -p "$home" "$bin" "$prefix/bin" "$prefix/node_modules/cursor-pkg"
  printf '%s\n' '{"name":"cursor-pkg","version":"1.0.0","bin":{"cursor-agent":"cli"}}' > "$prefix/node_modules/cursor-pkg/package.json"
  printf '#!/bin/sh\nexit 99\n' > "$prefix/node_modules/cursor-pkg/cli"
  chmod +x "$prefix/node_modules/cursor-pkg/cli"
  ln -s "$prefix/node_modules/cursor-pkg/cli" "$bin/cursor-agent"
  cat > "$bin/npm" <<'EOF_OPTIONAL_NPM'
#!/usr/bin/env bash
case "$1" in
  root) printf '%s\n' "$OMS_TEST_NPM_PREFIX/node_modules" ;;
  prefix) printf '%s\n' "$OMS_TEST_NPM_PREFIX" ;;
  view) case "$*" in *versions) printf '%s\n' '["2.0.0"]' ;; *) printf '%s\n' '{"2.0.0":"2020-01-01T00:00:00Z"}' ;; esac ;;
  install) printf '%s\n' "$*" >> "$OMS_TEST_NPM_PREFIX/calls"; mkdir -p "$OMS_TEST_NPM_PREFIX/node_modules/cursor-pkg"; printf '#!/bin/sh\nexit 99\n' > "$OMS_TEST_NPM_PREFIX/node_modules/cursor-pkg/cli"; chmod +x "$OMS_TEST_NPM_PREFIX/node_modules/cursor-pkg/cli"; printf '%s\n' '{"name":"cursor-pkg","version":"2.0.0","bin":{"cursor-agent":"cli"}}' > "$OMS_TEST_NPM_PREFIX/node_modules/cursor-pkg/package.json" ;;
  config) printf '%s\n' "$*" >> "$OMS_TEST_NPM_PREFIX/config-calls"; exit 1 ;;
  *) exit 1 ;;
esac
EOF_OPTIONAL_NPM
  chmod +x "$bin/npm"
  env -u OH_MY_SETTING_TOOL_LOCK -u NVM_DIR HOME="$home" PATH="$bin:/usr/bin:/bin" \
    OMS_TEST_NPM_PREFIX="$prefix" bash "$ROOT/scripts/install-tools.sh" --providers > "$TMP/optional-only.out" 2>&1 || fail "optional-only provider run failed"
  grep -Fq 'providers: none installed' "$TMP/optional-only.out" || fail "optional-only run detected a core provider"
  grep -Fq 'cursor-pkg@2.0.0' "$prefix/calls" || fail "optional-only run did not refresh"
  [ ! -e "$prefix/config-calls" ] || fail "optional-only run changed npm configuration"
)

# Doctor must surface a damaged repository lock before an install/repair tries
# to consume it. The standalone check keeps this regression independent from
# whatever provider CLIs happen to be installed on the test host.
python3 - "$LOCK" "$TMP/invalid-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["nvm"]["sha256"] = "not-a-digest"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if OH_MY_SETTING_TOOL_LOCK="$TMP/invalid-lock.json" \
    bash "$ROOT/scripts/doctor.sh" --tool-lock >"$TMP/doctor.out" 2>&1; then
  fail "doctor accepted a damaged tool lock"
fi
grep -Fq 'tool lock: invalid' "$TMP/doctor.out" || {
  sed -n '1,30p' "$TMP/doctor.out" >&2
  fail "doctor did not explain the damaged tool lock"
}

python3 - "$LOCK" "$TMP/invalid-native-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["npm"]["codex"]["native"]["linux-amd64"]["alias"] = "@openai/wrong"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-native-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a native payload bound to the wrong platform alias"
fi

python3 - "$LOCK" "$TMP/invalid-platform-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["gh"]["platforms"]["linux-amd64"]["archive"] = "zip"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-platform-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a payload archive type bound to the wrong platform"
fi

python3 - "$LOCK" "$TMP/swapped-platform-url-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
left = row["uv"]["platforms"]["linux-amd64"]["url"]
row["uv"]["platforms"]["linux-amd64"]["url"] = \
    row["uv"]["platforms"]["darwin-amd64"]["url"]
row["uv"]["platforms"]["darwin-amd64"]["url"] = left
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/swapped-platform-url-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted URLs swapped between platform rows"
fi

python3 - "$LOCK" "$TMP/wrong-host-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["uv"]["platforms"]["linux-amd64"]["url"] = \
    row["uv"]["platforms"]["linux-amd64"]["url"].replace(
        "https://github.com/", "https://example.test/"
    )
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/wrong-host-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted a platform payload from the wrong upstream host"
fi

python3 - "$LOCK" "$TMP/invalid-date-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["updated_at"] = "2026-02-31"
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
if python3 "$HELPER" --lock "$TMP/invalid-date-lock.json" validate \
    >/dev/null 2>&1; then
  fail "tool lock accepted an impossible update date"
fi

python3 - "$LOCK" <<'PY' || fail "tool lock contains floating or incomplete sources"
import json, re, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
assert row["schema"] == 2
assert re.fullmatch(r"\d+\.\d+\.\d+", row["python"]["version"])
assert re.fullmatch(r"v?\d+\.\d+\.\d+", row["node"]["version"])
assert row["node"]["platforms"]
for platform, item in row["node"]["platforms"].items():
    assert platform in {"darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"}, item
    assert item["url"].startswith("https://nodejs.org/dist/"), item
    assert re.fullmatch(r"[0-9a-f]{64}", item["sha256"]), item
assert re.fullmatch(r"[0-9a-f]{64}", row["nvm"]["sha256"]), row["nvm"]
assert re.fullmatch(r"[0-9a-f]{64}", row["nvm"]["script_sha256"]), row["nvm"]
assert set(row["uv"]["platforms"]) == {
    "darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64",
    "windows-amd64", "windows-arm64",
}
for platform, item in row["uv"]["platforms"].items():
    assert re.fullmatch(r"[0-9a-f]{64}", item["sha256"]), (platform, item)
for name, item in row["npm"].items():
    assert re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?", item["version"]), (name, item)
    assert item["integrity"].startswith("sha512-"), (name, item)
for name, expected in (("codex", 6), ("claude", 8)):
    native = row["npm"][name]["native"]
    assert len(native) == expected, (name, native)
    for platform, item in native.items():
        assert platform.startswith(("darwin-", "linux-", "windows-")), platform
        assert item["integrity"].startswith("sha512-"), (platform, item)
        assert item["alias"].startswith("@"), (platform, item)
assert "native" not in row["npm"]["ntn"]
for family in ("antigravity", "gh"):
    assert row[family]["platforms"], family
    for platform, item in row[family]["platforms"].items():
        key = "sha512" if family == "antigravity" else "sha256"
        assert re.fullmatch(r"[0-9a-f]{128}" if key == "sha512" else r"[0-9a-f]{64}", item[key]), (platform, item)
        assert item["url"].startswith("https://"), item
PY

# A clean but older user NVM is not a corrupt copy of the newly locked NVM.
# Preserve it, never source it, and activate the independently verified Node
# payload directly. This is the reinstall path that used to stop on every NVM
# lock bump before the receipt and auto-update wiring could be refreshed.
(
  export HOME="$TMP/older-nvm-home"
  export NVM_DIR="$HOME/.nvm"
  old_bin="$TMP/older-nvm-bin"
  mkdir -p "$NVM_DIR" "$old_bin"
  cat > "$NVM_DIR/nvm.sh" <<'EOF_NVM'
printf 'sourced\n' > "$HOME/nvm-was-sourced"
EOF_NVM
  printf '%s\n' '#!/usr/bin/env bash' 'echo v1.0.0' > "$old_bin/node"
  printf '%s\n' '#!/usr/bin/env bash' 'echo npm-old' > "$old_bin/npm"
  chmod +x "$old_bin/node" "$old_bin/npm"
  export PATH="$old_bin:$PATH"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  install_locked_node() {
    local target="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
    mkdir -p "$target"
    printf '%s\n' '#!/usr/bin/env bash' "echo v$NODE_VERSION" > "$target/node"
    printf '%s\n' '#!/usr/bin/env bash' 'echo npm-locked' > "$target/npm"
    chmod +x "$target/node" "$target/npm"
  }
  ensure_node >/dev/null || fail "an older NVM blocked locked Node activation"
  [ "$(command -v node)" = "$NVM_DIR/versions/node/v$NODE_VERSION/bin/node" ] ||
    fail "locked Node was not activated directly beside an older NVM"
  [ ! -e "$HOME/nvm-was-sourced" ] ||
    fail "an older NVM was sourced while activating locked Node"
)

# A fresh NVM install must bind both the archive and the extracted nvm.sh to
# their independent lock digests. Keep this behavioral: it catches an unset
# script digest before a downloaded shell file can be promoted.
(
  export HOME="$TMP/fresh-nvm-home"
  export NVM_DIR="$HOME/.nvm"
  mkdir -p "$HOME"
  # shellcheck source=scripts/install-tools.sh
  . "$ROOT/scripts/install-tools.sh"
  download_locked() { printf 'archive\n' > "$2"; }
  extract_locked() {
    mkdir -p "$3/nvm-${NVM_VERSION#v}"
    printf '#!/usr/bin/env bash\n' > "$3/nvm-${NVM_VERSION#v}/nvm.sh"
  }
  verify_digest() {
    [ -n "$2" ] || fail "fresh NVM install used an empty lock digest for $4"
  }
  install_nvm >/dev/null || fail "fresh locked NVM install failed"
  [ -f "$NVM_DIR/nvm.sh" ] || fail "fresh locked NVM was not promoted"
)

# A downloaded installer/archive whose bytes do not match the lock must never
# reach tar, bash, or another executable. This is the fail-closed regression
# for the old curl-pipe execution path.
mkdir -p "$TMP/bin" "$TMP/home"
python3 - "$LOCK" "$TMP/bad-lock.json" <<'PY'
import json, sys
row = json.load(open(sys.argv[1], encoding="utf-8"))
row["nvm"]["sha256"] = "a" * 64
json.dump(row, open(sys.argv[2], "w", encoding="utf-8"))
PY
cat > "$TMP/bin/curl" <<'EOF'
#!/usr/bin/env bash
out=""
while [ "$#" -gt 0 ]; do
  case "$1" in -o) out="$2"; shift 2 ;; *) shift ;; esac
done
printf 'untrusted payload\n' > "$out"
EOF
cat > "$TMP/bin/tar" <<'EOF'
#!/usr/bin/env bash
printf 'tar executed\n' > "$TOOL_LOCK_TAR_RAN"
exit 99
EOF
chmod +x "$TMP/bin/curl" "$TMP/bin/tar"

rc=0
env -u NVM_DIR HOME="$TMP/home" PATH="$TMP/bin:/usr/bin:/bin" \
  OH_MY_SETTING_TOOL_LOCK="$TMP/bad-lock.json" TOOL_LOCK_TAR_RAN="$TMP/tar-ran" \
  "$ROOT/scripts/install-tools.sh" >/dev/null 2>"$TMP/install.err" || rc=$?
[ "$rc" -ne 0 ] || fail "digest mismatch was accepted"
[ ! -e "$TMP/tar-ran" ] || fail "unverified nvm archive reached tar"
grep -Fq 'checksum mismatch' "$TMP/install.err" || {
  sed -n '1,20p' "$TMP/install.err" >&2
  fail "digest failure was not explained"
}

# Network-selected versions and pipe-to-shell execution are forbidden in the
# installer now that the repository owns an explicit lock.
if grep -En 'releases/latest|npm install -g "\$package"|curl[^|]*\|[[:space:]]*(sh|bash)' \
    "$ROOT/scripts/install-tools.sh" >/dev/null; then
  fail "install-tools still contains a floating or pipe-to-shell install"
fi
grep -Fq 'verify-sri' "$ROOT/scripts/install-tools.sh" ||
  fail "npm install bytes are not checked against locked integrity"
grep -Fq 'npm pack' "$ROOT/scripts/install-tools.sh" ||
  fail "npm packages are not staged before installation"
grep -Fq 'npm pack "$spec" --ignore-scripts' "$ROOT/scripts/install-tools.sh" ||
  fail "npm package staging can still run registry package lifecycle scripts"
if grep -Fq 'npm view "$spec" dist.integrity' "$ROOT/scripts/install-tools.sh"; then
  fail "npm integrity is still only a metadata preflight"
fi
grep -Fq 'node.platforms.$platform.sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "Node payload is not bound to the repository lock"
grep -Fq 'nvm.script_sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "an existing nvm.sh can be sourced without repository verification"
grep -Fq '.oh-my-setting-complete' "$ROOT/scripts/install-tools.sh" ||
  fail "interrupted Node/nvm staging has no completion marker for recovery"
if grep -Fq '. "$NVM_DIR/nvm.sh"' "$ROOT/install.sh"; then
  fail "the installer re-sources mutable nvm code after locked tools are installed"
fi
# A Windows checkout can hand the installer a CRLF lock; no scalar may carry
# the carriage return into a path, version, or digest.
sed 's/$/\r/' "$ROOT/tools.lock.json" > "$TMP/crlf.lock.json"
[ "$(bash -c '. "$1/scripts/lib/platform.sh"; oms_lock_scalar "$2" node.version' _ "$ROOT" "$TMP/crlf.lock.json")" = \
  "$(python3 "$ROOT/scripts/lib/tool-lock.py" --lock "$ROOT/tools.lock.json" get node.version)" ] ||
  fail "tool lock scalars from a CRLF lock are not normalized for native Windows"
grep -Fq 'uv.platforms.$platform.sha256' "$ROOT/scripts/install-tools.sh" ||
  fail "uv still executes a verified installer that selects an unverified payload"
if grep -En 'nvm[[:space:]]+install' "$ROOT/scripts/install-tools.sh" >/dev/null; then
  fail "Node still delegates payload selection to nvm instead of the lock"
fi
grep -Fq -- '--offline' "$ROOT/scripts/install-tools.sh" ||
  fail "verified npm tarballs can still trigger an unbounded registry fetch"
grep -Fq -- '--cache "$tmp/install-cache"' "$ROOT/scripts/install-tools.sh" ||
  fail "npm install can still reuse unrelated cached registry payloads"
grep -Fq 'npm.$name.native.$native_platform.integrity' \
  "$ROOT/scripts/install-tools.sh" ||
  fail "platform-native npm payloads are not selected from the lock"
grep -Fq 'install-npm-payload' "$ROOT/scripts/install-tools.sh" ||
  fail "verified native npm payloads are not installed through the safe helper"
shim_body="$(sed -n '/^write_npm_shim()/,/^}/p' "$ROOT/scripts/install-tools.sh")"
if printf '%s\n' "$shim_body" | grep >/dev/null -F 'nvm.sh'; then
  fail "provider shims source mutable nvm code without revalidating it"
fi

# Timer PATH discovery is read-only and does not source shell/nvm startup.
(
  export HOME="$TMP/timer-home"
  export NVM_DIR="$HOME/.nvm"
  set --
  . "$ROOT/scripts/install-tools.sh"
  node_dir="$NVM_DIR/versions/node/v$NODE_VERSION/bin"
  mkdir -p "$node_dir" "$HOME/.local/bin"
  for binary in node npm; do
    printf '#!/bin/sh\nexit 0\n' > "$node_dir/$binary"
    chmod +x "$node_dir/$binary"
  done
  printf '#!/bin/sh\nexit 0\n' > "$HOME/.local/bin/codex"
  chmod +x "$HOME/.local/bin/codex"
  PATH=/usr/bin:/bin
  activate_provider_paths
  [ "$(command -v codex)" = "$HOME/.local/bin/codex" ] || fail "timer missed local provider"
  [ "$(command -v npm)" = "$node_dir/npm" ] || fail "timer missed installed npm"
  [ ! -e "$HOME/.profile" ] || fail "provider discovery edited shell configuration"
)

provider_snapshot_case

python3 -m unittest discover -s "$ROOT/tests" -p test_provider_latest.py ||
  fail "provider release resolver regression"

echo "tool-lock-smoke: ok"
