#!/usr/bin/env bash

# Pure PROJECT.md publication-state parser. Callers keep their own authority;
# this only gives every read/preflight surface the same typed interpretation.
oms_project_state() {  # FILE -> missing|draft|confirmed|legacy-active|invalid
  local file="$1"
  [ -f "$file" ] || { printf 'missing\n'; return 0; }
  awk '
    {
      sub(/\r$/, "", $0)
      if (index($0, "- State:") != 1) next
      value = substr($0, length("- State:") + 1)
      sub(/^[[:space:]]*/, "", value)
      sub(/[[:space:]]*$/, "", value)
      count++
      if (count == 1) state = value
    }
    END {
      if (count == 0) print "missing"
      else if (count != 1) print "invalid"
      else if (state == "draft") print "draft"
      else if (state == "confirmed") print "confirmed"
      else if (state == "active") print "legacy-active"
      else print "invalid"
    }
  ' "$file"
}
