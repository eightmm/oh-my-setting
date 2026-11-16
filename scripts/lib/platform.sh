#!/usr/bin/env bash
# shellcheck shell=bash

# Small platform boundary shared by installers, entrypoints, and lifecycle tools.
# Windows means a POSIX shell supplied by Git for Windows/MSYS2/Cygwin; WSL
# reports Linux and follows the normal Linux path.

[ -n "${OMS_PLATFORM_KERNEL:-}" ] ||
  OMS_PLATFORM_KERNEL="$(uname -s 2>/dev/null || printf unknown)"
[ -n "${OMS_PLATFORM_MACHINE:-}" ] ||
  OMS_PLATFORM_MACHINE="$(uname -m 2>/dev/null || printf unknown)"

oms_platform_name() {
  local value="${OMS_PLATFORM_OVERRIDE:-}"

  if [ -z "$value" ]; then
    value="$OMS_PLATFORM_KERNEL"
  fi
  case "$value" in
    windows|Windows|WINDOWS|MINGW*|MSYS*|CYGWIN*) printf 'windows\n' ;;
    macos|macOS|MacOS|Darwin) printf 'macos\n' ;;
    linux|Linux) printf 'linux\n' ;;
    *) printf 'unknown\n' ;;
  esac
}

oms_install_link_mode() {
  local mode="${OH_MY_SETTING_LINK_MODE:-auto}"

  case "$mode" in
    auto) printf 'symlink\n' ;;
    copy|symlink)
      printf '%s\n' "$mode"
      ;;
    *)
      echo "error: OH_MY_SETTING_LINK_MODE must be auto, copy, or symlink" >&2
      return 2
      ;;
  esac
}

# The locked tool archive for this host, e.g. linux-amd64.
oms_tool_platform() {
  local os arch
  case "$OMS_PLATFORM_KERNEL" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) echo "error: unsupported tool platform: $OMS_PLATFORM_KERNEL" >&2; return 1 ;;
  esac
  case "$OMS_PLATFORM_MACHINE" in
    x86_64|amd64) arch=amd64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) echo "error: unsupported tool architecture: $OMS_PLATFORM_MACHINE" >&2; return 1 ;;
  esac
  printf '%s-%s\n' "$os" "$arch"
}

oms_sha256_file() {
  [ -f "$1" ] || return 1
  if command -v sha256sum >/dev/null 2>&1; then sha256sum < "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 < "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then openssl dgst -sha256 < "$1" | awk '{print $NF}'
  else echo "error: sha256sum, shasum, or openssl is required" >&2; return 1
  fi
}

# Read one scalar from a flat or nested JSON object by dotted path using pure awk.
oms_lock_scalar() {  # oms_lock_scalar JSON_FILE DOTTED.PATH
  awk -v want="$2" '
    { text = text $0 "\n" }
    END {
      if (index(text, "\\") != 0) exit 1
      n = length(text); depth = 0; lists = 0; key = ""; value = 0
      for (i = 1; i <= n; i++) {
        c = substr(text, i, 1)
        if (c == "\"") {
          s = ""
          for (i++; i <= n && (d = substr(text, i, 1)) != "\""; i++) {
            if (d == "\\") { i++; d = substr(text, i, 1) }
            s = s d
          }
          if (lists) continue
          if (value) {
            if (path(key) == want) { print s; found = 1; exit }
            value = 0
          } else key = s
        } else if (c == ":") value = 1
        else if (c == "{") { name[++depth] = value ? key : ""; value = 0 }
        else if (c == "}") depth--
        else if (c == "[") lists++
        else if (c == "]") lists--
        else if (c == ",") value = 0
        else if (value && !lists && c ~ /[-0-9tfn]/) {
          s = c
          while (i < n && index(",}] \t\r\n", substr(text, i + 1, 1)) == 0) s = s substr(text, ++i, 1)
          if (path(key) == want) { print s; found = 1; exit }
          value = 0
        }
      }
    }
    function path(leaf,   p, k) {
      p = ""
      for (k = 2; k <= depth; k++) p = p name[k] "."
      return p leaf
    }
    END { exit found ? 0 : 1 }
  ' "$1"
}

# Children started through `#!/usr/bin/env bash` resolve the first bash on PATH.
# Under Bash 5, name this interpreter for them and prepend its directory when needed.
oms_bash_enter() {
  local first

  [ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || return 0
  case "${BASH:-}" in /*) ;; *) return 0 ;; esac
  export OMS_BASH="$BASH"
  first="$(command -v bash 2>/dev/null || true)"
  if [ -n "$first" ] && [ "$first" -ef "$BASH" ]; then
    return 0
  fi
  PATH="${BASH%/*}:$PATH"
  export PATH
  hash -r 2>/dev/null || true
}

# Resolve the verified current-revision native oms payload without building or
# modifying files. Return 0 when verified; 1 when no entry exists for this commit;
# 2 with OMS_NATIVE_ERROR when present state cannot be trusted.
oms_native_resolve() {  # oms_native_resolve ROOT
  local root="$1" cache revision entry binary checksum dir size expected actual
  cache="$root/rust/target/oms-native"
  OMS_NATIVE_BINARY="" OMS_NATIVE_ROOT="" OMS_NATIVE_REVISION="" OMS_NATIVE_ERROR=""
  [ -e "$cache" ] || [ -L "$cache" ] || return 1
  for dir in "$root/rust" "$root/rust/target" "$cache"; do
    if [ -L "$dir" ] || [ ! -d "$dir" ]; then
      OMS_NATIVE_ERROR="foreign native payload directory: $dir"
      return 2
    fi
  done
  revision="$(unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX; git -C "$root" rev-parse --verify HEAD 2>/dev/null)" || {
    OMS_NATIVE_ERROR='cannot determine native payload revision'
    return 2
  }
  case "$revision" in *[!0-9a-f]*|'') OMS_NATIVE_ERROR='invalid native payload revision'; return 2 ;; esac
  [ "${#revision}" = 40 ] || { OMS_NATIVE_ERROR='invalid native payload revision'; return 2; }
  entry="$cache/$revision"
  [ -e "$entry" ] || [ -L "$entry" ] || return 1
  binary="$entry/oms"
  checksum="$entry/oms.sha256"
  if [ -L "$entry" ] || [ ! -d "$entry" ] || [ -L "$binary" ]; then
    OMS_NATIVE_ERROR="foreign native payload entry: $entry"
    return 2
  fi
  if [ -L "$checksum" ]; then
    OMS_NATIVE_ERROR="foreign native checksum: $checksum"
    return 2
  fi
  if [ ! -f "$binary" ] || [ ! -x "$binary" ]; then
    OMS_NATIVE_ERROR='native payload unavailable; run oms doctor --repair'
    return 2
  fi
  if [ ! -f "$checksum" ]; then
    OMS_NATIVE_ERROR='native executable checksum unavailable; run oms doctor --repair'
    return 2
  fi
  if [ ! -O "$root/rust" ] || [ ! -O "$root/rust/target" ] || [ ! -O "$cache" ] ||
     [ ! -O "$entry" ] || [ ! -O "$binary" ] || [ ! -O "$checksum" ] ||
     [ -n "$(find "$root/rust" "$root/rust/target" "$cache" "$entry" "$binary" "$checksum" -prune -perm -0002 -print 2>/dev/null)" ]; then
    OMS_NATIVE_ERROR="untrusted native payload ownership: $entry"
    return 2
  fi
  size=""
  size="$(LC_ALL=C wc -c < "$checksum" 2>/dev/null)" || size=""
  size="${size//[[:space:]]/}"
  expected=""
  if [ "$size" != 65 ] || ! IFS= read -r expected < "$checksum"; then
    OMS_NATIVE_ERROR='native executable checksum unavailable; run oms doctor --repair'
    return 2
  fi
  case "$expected" in *[!0-9a-f]*|'') OMS_NATIVE_ERROR='invalid native executable checksum'; return 2 ;; esac
  if [ "${#expected}" -ne 64 ] ||
     ! actual="$(oms_sha256_file "$binary" 2>/dev/null)" ||
     [ "$actual" != "$expected" ]; then
    OMS_NATIVE_ERROR='native executable checksum mismatch; run oms doctor --repair'
    return 2
  fi
  OMS_NATIVE_BINARY="$binary" OMS_NATIVE_ROOT="$root" OMS_NATIVE_REVISION="$revision"
}

# Quarantine an invalid or symlinked rust/target/oms-native/<revision> entry so
# `oms doctor --repair` can re-stage a verified payload cleanly.
oms_native_quarantine() {  # oms_native_quarantine ROOT
  local root="$1" cache revision entry quarantine
  cache="$root/rust/target/oms-native"
  revision="$(unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX; git -C "$root" rev-parse --verify HEAD 2>/dev/null)" || return 1
  case "$revision" in *[!0-9a-f]*|'') return 1 ;; esac
  [ "${#revision}" = 40 ] || return 1
  entry="$cache/$revision"
  [ -e "$entry" ] || [ -L "$entry" ] || return 0
  if oms_native_resolve "$root" >/dev/null 2>&1; then
    return 0
  fi
  quarantine="$cache/.broken-$revision-$$-$(date +%s)"
  mv -f "$entry" "$quarantine"
}

# Build and publish verified release binaries into rust/target/oms-native/<HEAD>.
oms_native_stage() {  # oms_native_stage ROOT
  local root="$1" cache revision target_dir release_dir stage_dir entry
  local artifacts="" oms_digest="" name digest
  [ -f "$root/rust/Cargo.toml" ] || return 1
  command -v cargo >/dev/null 2>&1 || return 1
  revision="$(unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX; git -C "$root" rev-parse --verify HEAD 2>/dev/null)" || return 1
  case "$revision" in *[!0-9a-f]*|'') return 1 ;; esac
  [ "${#revision}" = 40 ] || return 1
  cache="$root/rust/target/oms-native"
  entry="$cache/$revision"
  if [ -e "$entry" ] || [ -L "$entry" ]; then
    if [ "${OMS_NATIVE_FORCE_STAGE:-0}" != 1 ] && oms_native_resolve "$root" >/dev/null 2>&1; then
      return 0
    fi
    if [ "${OMS_NATIVE_FORCE_STAGE:-0}" != 1 ]; then
      return 2
    fi
  fi
  mkdir -p "$root/rust/target" "$cache"
  chmod go-w "$root/rust" "$root/rust/target" "$cache" 2>/dev/null || true
  target_dir="$root/rust/target/lifecycle-build"
  release_dir="$target_dir/release"
  cargo +1.99.1 build --manifest-path "$root/rust/Cargo.toml" \
    --locked --offline --release \
    --bin oms \
    --bin oms-mcp-peer-owner \
    --bin oms-run-bounded-helper \
    --bin oms-state-queue-worker \
    --bin oms-graph-python-helper \
    --target-dir "$target_dir" >&2 || return 1
  stage_dir="$(mktemp -d "$cache/.stage-$revision.XXXXXX")" || return 1
  chmod 0755 "$stage_dir"
  for name in oms oms-mcp-peer-owner oms-run-bounded-helper oms-state-queue-worker oms-graph-python-helper; do
    [ -f "$release_dir/$name" ] && [ -x "$release_dir/$name" ] || { rm -rf "$stage_dir"; return 1; }
    cp -f "$release_dir/$name" "$stage_dir/$name" && chmod 0755 "$stage_dir/$name" || { rm -rf "$stage_dir"; return 1; }
    digest="$(oms_sha256_file "$stage_dir/$name")" || { rm -rf "$stage_dir"; return 1; }
    [ "$name" != oms ] || oms_digest="$digest"
    artifacts="$artifacts${artifacts:+,}\"$name\":\"$digest\""
  done
  printf '%s\n' "$oms_digest" > "$stage_dir/oms.sha256"
  printf '{"schema":1,"source_commit":"%s","artifacts":{%s}}\n' "$revision" "$artifacts" > "$stage_dir/manifest.json"
  chmod 0600 "$stage_dir/oms.sha256" "$stage_dir/manifest.json"
  if [ -e "$entry" ] || [ -L "$entry" ]; then
    if [ "${OMS_NATIVE_FORCE_STAGE:-0}" = 1 ]; then
      rm -rf "$entry" || { rm -rf "$stage_dir"; return 1; }
    else
      rm -rf "$stage_dir"
      oms_native_resolve "$root" >/dev/null 2>&1
      return $?
    fi
  fi
  mv -T "$stage_dir" "$entry" 2>/dev/null || mv "$stage_dir" "$entry" || { rm -rf "$stage_dir"; return 1; }
}

# oms_native_route POLICY ROOT VERB [ARG...]
# Execs the verified native `oms VERB ARG...` when the payload selects VERB.
# Auto-stages a missing payload when cargo is available (unless disabled via
# OMS_NATIVE_NO_AUTO_STAGE=1) and quarantines corrupt entries on `doctor --repair`.
# Returns 0 with OMS_NATIVE_OUTCOME (absent|declined|unverified) when unresolved
# under recover/hook or when unbuilt without cargo; returns 2 under strict corruption.
# shellcheck disable=SC2034 # OMS_NATIVE_OUTCOME is the caller's result.
oms_native_route() {
  local policy="$1" root="$2" status=0 arg has_repair=0
  shift 2
  OMS_NATIVE_OUTCOME=absent
  oms_native_resolve "$root" || status=$?
  if [ "$status" -eq 1 ] && [ "${OMS_NATIVE_NO_AUTO_STAGE:-0}" != 1 ] &&
     [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ -f "$root/rust/Cargo.toml" ] &&
     command -v cargo >/dev/null 2>&1; then
    oms_native_stage "$root" || true
    status=0
    oms_native_resolve "$root" || status=$?
  elif [ "$status" -ge 2 ] && [ "$policy" = recover ] && [ "${1:-}" = doctor ]; then
    for arg in "${@:2}"; do
      [ "$arg" = "--repair" ] && has_repair=1
    done
    if [ "$has_repair" = 1 ] && [ "${OMS_NATIVE_NO_AUTO_STAGE:-0}" != 1 ] &&
       [ "${OMS_HARNESS_CHILD:-0}" != 1 ] && [ -f "$root/rust/Cargo.toml" ] &&
       command -v cargo >/dev/null 2>&1; then
      oms_native_quarantine "$root" && oms_native_stage "$root" || true
      status=0
      oms_native_resolve "$root" || status=$?
    fi
  fi
  case "$status" in
    0) ;;
    1) return 0 ;;
    *)
      OMS_NATIVE_OUTCOME=unverified
      case "$policy" in
        strict) echo "error: $OMS_NATIVE_ERROR" >&2; return 2 ;;
        recover) echo "warning: $OMS_NATIVE_ERROR" >&2; return 0 ;;
        *) return 0 ;;
      esac
      ;;
  esac
  if [ "$policy" = hook ]; then
    "$OMS_NATIVE_BINARY" --oms-select "$OMS_NATIVE_ROOT" "$OMS_NATIVE_REVISION" -- "$@" </dev/null >/dev/null 2>&1 || status=$?
  else
    "$OMS_NATIVE_BINARY" --oms-select "$OMS_NATIVE_ROOT" "$OMS_NATIVE_REVISION" -- "$@" </dev/null >/dev/null || status=$?
  fi
  case "$status" in
    0) exec "$OMS_NATIVE_BINARY" --oms-dispatch "$OMS_NATIVE_ROOT" "$OMS_NATIVE_REVISION" -- "$@" ;;
    75) OMS_NATIVE_OUTCOME=declined; return 0 ;;
  esac
  OMS_NATIVE_OUTCOME=unverified
  [ "$status" -lt 128 ] || return "$status"
  [ "$policy" = strict ] && return "$status"
  return 0
}
