#!/usr/bin/env bash
set -euo pipefail

# Build once on a release host; recipients keep their source checkout and import
# this exact native payload with update --prepare --from FILE --sha256 DIGEST.
# The digest is an out-of-band trust input, not a signature or a CI attestation.

usage() {
  cat <<'EOF'
Usage: scripts/package-native-release.sh --output FILE.tar.gz

Package the current clean commit's verified native payload for this host.
Reuse its staged payload when present; otherwise build with its Rust toolchain.
The output is created once and never overwrites an existing file. Publish the
printed SHA-256 through a trusted channel before importing the bundle.
EOF
}

OUTPUT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      [ "$#" -ge 2 ] && [ -n "$2" ] || { usage >&2; exit 2; }
      OUTPUT="$2"
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 2 ;;
  esac
done
[ -n "$OUTPUT" ] || { usage >&2; exit 2; }
[ "${BASH_VERSINFO[0]:-0}" -ge 5 ] || { echo 'error: Bash 5 or newer is required' >&2; exit 2; }

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
unset GIT_DIR GIT_WORK_TREE GIT_COMMON_DIR GIT_INDEX_FILE GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_PREFIX
REVISION="$(git -C "$ROOT" rev-parse --verify HEAD)"
case "$REVISION" in ''|*[!0-9a-f]*) echo 'error: invalid source revision' >&2; exit 2 ;; esac
[ "${#REVISION}" -eq 40 ] || { echo 'error: unsupported source revision' >&2; exit 2; }
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ] || {
  echo 'error: release packaging requires a clean source checkout' >&2
  exit 2
}

case "$(uname -s)" in
  Linux) KERNEL=linux ;;
  Darwin) KERNEL=darwin ;;
  *) echo 'error: unsupported native bundle host' >&2; exit 2 ;;
esac
case "$(uname -m)" in
  x86_64|amd64) ARCH=amd64 ;;
  aarch64|arm64) ARCH=arm64 ;;
  *) echo 'error: unsupported native bundle architecture' >&2; exit 2 ;;
esac
if [ "$KERNEL" = linux ] && compgen -G '/lib/ld-musl-*' >/dev/null; then
  echo 'error: native bundles require glibc Linux or macOS' >&2
  exit 2
fi

# shellcheck source=scripts/lib/platform.sh
. "$ROOT/scripts/lib/platform.sh"
oms_bash_enter
resolve_status=0
oms_native_resolve "$ROOT" || resolve_status=$?
case "$resolve_status" in
  0) ;;
  1) oms_native_stage "$ROOT"; oms_native_resolve "$ROOT" ;;
  *) echo "error: ${OMS_NATIVE_ERROR:-native payload verification failed}" >&2; exit 2 ;;
esac
"$OMS_NATIVE_BINARY" --oms-dispatch "$ROOT" "$REVISION" -- link --plan >/dev/null

case "$OUTPUT" in /*) ;; *) OUTPUT="$PWD/$OUTPUT" ;; esac
[ ! -e "$OUTPUT" ] && [ ! -L "$OUTPUT" ] || { echo 'error: output already exists' >&2; exit 2; }
mkdir -p "$(dirname "$OUTPUT")"
WORK="$(mktemp -d "$ROOT/target/.native-bundle.XXXXXX")"
PACKED=""
cleanup() {
  rm -rf -- "$WORK"
  [ -z "$PACKED" ] || rm -f -- "$PACKED"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir "$WORK/payload"
NATIVE="$ROOT/target/oms-native/$REVISION"
for name in oms oms-mcp-peer-owner oms-run-bounded-helper oms-state-queue-worker oms-graph-python-helper manifest.json oms.sha256; do
  [ -f "$NATIVE/$name" ] && [ ! -L "$NATIVE/$name" ] || { echo "error: invalid native member: $name" >&2; exit 2; }
  cp "$NATIVE/$name" "$WORK/payload/$name"
done
if [ -e "$NATIVE/oms-supervisor" ] || [ -L "$NATIVE/oms-supervisor" ]; then
  [ -f "$NATIVE/oms-supervisor" ] && [ ! -L "$NATIVE/oms-supervisor" ] || { echo 'error: invalid supervisor' >&2; exit 2; }
  cp "$NATIVE/oms-supervisor" "$WORK/payload/oms-supervisor"
fi
for artifact in "$WORK"/payload/oms*; do
  name="${artifact##*/}"
  [ "$name" != oms.sha256 ] || continue
  expected="$(oms_lock_scalar "$WORK/payload/manifest.json" "artifacts.$name")"
  [ -x "$artifact" ] && [ "$(oms_sha256_file "$artifact")" = "$expected" ] || {
    echo "error: copied native artifact failed verification: $name" >&2
    exit 2
  }
done
printf '{"schema":1,"kind":"oms-native-payload","source_commit":"%s","platform":"%s-%s"}\n' \
  "$REVISION" "$KERNEL" "$ARCH" > "$WORK/bundle.json"

# Recheck the source before publication; source proof and checksums are checked
# again by the receiving updater before the payload may run.
[ "$(git -C "$ROOT" rev-parse --verify HEAD)" = "$REVISION" ] &&
  [ -z "$(git -C "$ROOT" status --porcelain --untracked-files=normal)" ] || {
    echo 'error: source changed while packaging' >&2
    exit 2
  }
PACKED="$(mktemp "$OUTPUT.tmp.XXXXXX")"
COPYFILE_DISABLE=1 tar --format=ustar -czf "$PACKED" -C "$WORK" bundle.json payload
DIGEST="$(oms_sha256_file "$PACKED")"
# Same-directory temporary file permits atomic, no-clobber publication on both
# GNU and BSD systems. Removing the temporary name leaves a single-link bundle.
ln "$PACKED" "$OUTPUT"
rm "$PACKED"
PACKED=""
printf 'bundle: %s\nsha256: %s\nsource_commit: %s\nplatform: %s-%s\n' \
  "$OUTPUT" "$DIGEST" "$REVISION" "$KERNEL" "$ARCH"
