#!/usr/bin/env bash

# Preserve shell path spelling for cksum, including on Git Bash.
oms_land_state_dir() {  # REPO
  local repo="$1" common_dir common_root name digest
  common_dir="$(git -C "$repo" rev-parse --git-common-dir 2>/dev/null | tr -d '\r')" || return 1
  case "$common_dir" in
    /*|[A-Za-z]:/*) ;;
    *) common_dir="$repo/$common_dir" ;;
  esac
  common_dir="$(cd "$common_dir" 2>/dev/null && pwd -P)" || return 1
  common_root="$(cd "$common_dir/.." 2>/dev/null && pwd -P)" || return 1
  name="$(basename "$common_root")"
  name="$(printf '%s' "$name" | tr -c 'A-Za-z0-9._-' '_')"
  name="${name//../_}"
  case "$name" in ''|.|..) name=repo ;; esac
  digest="$(printf '%s' "$common_root" | cksum | awk '{print $1 "-" $2}')"
  case "$digest" in ''|*[!0-9-]*) return 1 ;; esac
  printf '%s/%s-%s\n' "${XDG_STATE_HOME:-$HOME/.local/state}/oh-my-setting/land" "$name" "$digest"
}
