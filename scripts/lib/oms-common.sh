# shellcheck shell=bash
# Cross-cutting primitives shared by the run tools. Sourced, not executed.

# Effective run id for auto-linking: explicit OMS_RUN_ID wins; otherwise the
# repo's .oms/runs/CURRENT pointer (written by oms-run.sh new) when it is
# fresh. A stale pointer must not misjoin unrelated later work, so it expires
# after OMS_RUN_CURRENT_TTL seconds (default 86400, same as board claims).
# Prints nothing and returns nonzero when neither applies.
oms_effective_run_id() {
  local state_root="$1"
  local current id minted now ttl

  if [ -n "${OMS_RUN_ID:-}" ]; then
    printf '%s\n' "$OMS_RUN_ID"
    return 0
  fi
  current="$state_root/.oms/runs/CURRENT"
  [ -f "$current" ] || return 1
  id="$(awk 'NR==1{print $1}' "$current")"
  minted="$(awk 'NR==1{print $2}' "$current")"
  [ -n "$id" ] || return 1
  case "$minted" in *[!0-9]*|"") return 1 ;; esac
  ttl="${OMS_RUN_CURRENT_TTL:-86400}"
  case "$ttl" in *[!0-9]*|"") ttl=86400 ;; esac
  now="$(date +%s)"
  [ $((now - minted)) -le "$ttl" ] || return 1
  printf '%s\n' "$id"
}

# Hash stdin / a file with whatever sha256 tool exists. Returns nonzero (no
# output) when none is available, so callers can compose without aborting.
oms_sha256_stream() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 | awk '{print $NF}'
  else
    return 1
  fi
}

oms_sha256_file() {
  [ -f "$1" ] || return 1
  oms_sha256_stream < "$1"
}
