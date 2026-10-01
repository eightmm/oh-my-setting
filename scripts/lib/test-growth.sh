#!/usr/bin/env bash
# shellcheck shell=bash

# Net test and code lines between two commits, shown before a push or an
# affected check. Every test added here looked justified on its own, yet tests
# outgrew code (0.54 to 0.70 of code lines, 2026-08 to 2026-10); the total is
# the signal no single change shows. Docs and images count as neither.
oms_test_growth() {  # oms_test_growth FROM TO [REPO]
  git -C "${3:-.}" diff --numstat "$1" "$2" 2>/dev/null | awk '
    $1 ~ /^[0-9]+$/ {
      d = $1 - $2
      if ($3 ~ /(^|\/)(tests?|spec)\// || $3 ~ /(^|\/)test_[^\/]*$/ || $3 ~ /_test\.[A-Za-z]+$/) t += d
      else if ($3 !~ /(^|\/)docs\// && $3 !~ /\.(md|svg|png|jpe?g|gif)$/) c += d
    }
    END { printf "tests %+d / code %+d lines\n", t, c }'
}
