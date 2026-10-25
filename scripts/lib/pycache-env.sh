#!/usr/bin/env bash
# shellcheck shell=bash

# Bytecode policy for the Python-launching wrappers: cache under an OMS-owned
# directory only when it resolves outside the current repository and the OMS
# source tree being run; otherwise keep bytecode disabled so neither gains
# __pycache__. Dedicated file: the wrappers run on hook paths and must not
# source a large library for this.

oms_pycache_outside() {  # PATH ROOT
  local path="$1" tail="" probe next here resolved guard
  case "$path" in /*|?:*) ;; *) return 1 ;; esac
  case "/$path/" in */../*|*/./*) return 1 ;; esac
  # Resolve the deepest existing ancestor so a symlinked root is judged by its
  # real location before anything is created.
  probe="$path"
  while [ ! -d "$probe" ]; do
    next="${probe%/*}"
    [ "$next" != "$probe" ] || return 1
    tail="/${probe##*/}$tail"
    probe="${next:-/}"
  done
  resolved="$(cd "$probe" 2>/dev/null && pwd -P)" || return 1
  resolved="${resolved%/}$tail/"
  guard="$(cd "$2" 2>/dev/null && pwd -P)" || return 1
  case "$resolved" in "${guard%/}"/*) return 1 ;; esac
  here="$(pwd -P)" || return 1
  while :; do
    if [ -e "$here/.git" ]; then
      case "$resolved" in "$here"/*) return 1 ;; esac
      break
    fi
    next="${here%/*}"
    [ "$next" != "$here" ] || break
    here="$next"
  done
}

oms_pycache_env() {  # ROOT: the OMS source tree being run
  local want="${PYTHONPYCACHEPREFIX:-}"
  if [ -z "$want" ]; then
    want="${XDG_CACHE_HOME:-}"
    case "$want" in /*|?:*) ;; *) want="${HOME:+$HOME/.cache}" ;; esac
    [ -z "$want" ] || want="$want/oms/pycache"
  fi
  if [ -n "$want" ] && oms_pycache_outside "$want" "$1" && mkdir -p "$want" 2>/dev/null; then
    export PYTHONPYCACHEPREFIX="$want"
  else
    unset PYTHONPYCACHEPREFIX
    export PYTHONDONTWRITEBYTECODE=1
  fi
}
