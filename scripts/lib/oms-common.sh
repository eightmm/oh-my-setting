# shellcheck shell=bash
# Cross-cutting primitives shared by the run tools. Sourced, not executed.

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
