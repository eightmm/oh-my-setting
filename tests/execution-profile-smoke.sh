#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-execution-profile.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "execution-profile-smoke: $*" >&2; exit 1; }
PROFILE="$ROOT/scripts/execution-profile.sh"

[ -x "$PROFILE" ] || fail "missing executable: scripts/execution-profile.sh"

"$PROFILE" describe --profile trusted-local --json > "$TMP/trusted.json"
python3 - "$TMP/trusted.json" <<'PY' || fail "trusted-local description is invalid"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["schema"] == 1 and d["profile"] == "trusted-local", d
assert d["isolation"] == "none", d
assert d["automatic_install"] is False, d
assert d["requirements"]["commands"] == ["bash", "git", "python3"], d
assert "host user" in d["description"].lower(), d
PY

"$PROFILE" describe --profile isolated --json > "$TMP/isolated-description.json"
python3 - "$TMP/isolated-description.json" <<'PY' || fail "isolated requirements are incomplete"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["requirements"]["commands"] == ["bash", "git", "python3", "docker"], d
names = {row["name"] for row in d["requirements"]["environment"]}
assert names == {"OMS_ISOLATED_IMAGE", "OMS_DOCKER_BIN"}, d
assert d["constraints"]["local_image_required"] is True, d
PY

"$PROFILE" describe --profile remote --json > "$TMP/remote-description.json"
python3 - "$TMP/remote-description.json" <<'PY' || fail "remote requirements are incomplete"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["constraints"]["adapter_owns_transport"] is True, d
assert d["constraints"]["adapter_owns_authentication"] is True, d
PY

"$PROFILE" check --profile trusted-local --repo "$ROOT" --json > "$TMP/trusted-check.json"
python3 - "$TMP/trusted-check.json" <<'PY' || fail "trusted-local check failed"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["ok"] is True and d["profile"] == "trusted-local", d
assert d["repo"] and d["requirements"]["commands"]["git"] is True, d
PY

set +e
OMS_DOCKER_BIN="$TMP/does-not-exist" \
  "$PROFILE" check --profile isolated --repo "$ROOT" --image oms/test:latest \
  > "$TMP/no-docker.out" 2> "$TMP/no-docker.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "isolated without Docker should exit 2, got $rc"
grep -Fq 'isolated profile requires Docker' "$TMP/no-docker.err" ||
  fail "missing Docker failure is not explicit"
grep -Eiq 'install|Docker' "$TMP/no-docker.err" ||
  fail "missing Docker failure does not explain the requirement"

mkdir -p "$TMP/bin"
apply_log="$TMP/docker.argv"
export OMS_TEST_DOCKER_LOG="$apply_log"
cat > "$TMP/bin/docker-fake" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$OMS_TEST_DOCKER_LOG"
if [ "${1:-}" = info ]; then
  printf '"27.0.0"\n'
  exit 0
fi
if [ "${1:-}" = image ] && [ "${2:-}" = inspect ]; then
  printf '"sha256:test"\n'
  exit 0
fi
exit 9
EOF
chmod +x "$TMP/bin/docker-fake"

OMS_DOCKER_BIN="$TMP/bin/docker-fake" \
  "$PROFILE" check --profile isolated --repo "$ROOT" --image oms/test:latest --json \
  > "$TMP/isolated.json"
python3 - "$TMP/isolated.json" <<'PY' || fail "isolated check is invalid"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["ok"] is True and d["profile"] == "isolated", d
assert d["isolation"] == "docker", d
assert d["image"] == "oms/test:latest", d
assert d["requirements"]["docker_client"] is True, d
assert d["requirements"]["docker_daemon"] is True, d
PY
[ "$(wc -l < "$apply_log" | tr -d ' ')" -eq 2 ] ||
  fail "isolated check should probe the Docker daemon and image exactly once"
grep -Fxq 'info' "$apply_log" ||
  fail "isolated check used an unexpected Docker probe"
grep -Fxq 'image inspect oms/test:latest' "$apply_log" ||
  fail "isolated check did not verify the explicit local image"
if grep -Eiq 'install|pull|run' "$apply_log"; then
  fail "isolated check attempted to install, pull, or run Docker content"
fi

cat > "$TMP/bin/docker-no-image" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "$1" = info ]; then printf '"27.0.0"\n'; exit 0; fi
if [ "$1" = image ] && [ "$2" = inspect ]; then exit 1; fi
exit 9
EOF
chmod +x "$TMP/bin/docker-no-image"
set +e
OMS_DOCKER_BIN="$TMP/bin/docker-no-image" \
  "$PROFILE" check --profile isolated --repo "$ROOT" --image missing:test \
  > "$TMP/no-image.out" 2> "$TMP/no-image.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "missing isolated image should exit 2, got $rc"
grep -Fq 'isolated image is not available locally: missing:test' "$TMP/no-image.err" ||
  fail "missing isolated image failure is not explicit"
grep -Fq 'pull or build it explicitly' "$TMP/no-image.err" ||
  fail "missing image failure does not preserve operator ownership"

set +e
"$PROFILE" check --profile remote --repo "$ROOT" \
  > "$TMP/no-remote.out" 2> "$TMP/no-remote.err"
rc=$?
set -e
[ "$rc" -eq 2 ] || fail "remote without adapter should exit 2, got $rc"
grep -Fq 'remote profile requires an executable adapter' "$TMP/no-remote.err" ||
  fail "remote adapter requirement is not explicit"

cat > "$TMP/bin/remote-adapter" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$TMP/bin/remote-adapter"
"$PROFILE" check --profile remote --repo "$ROOT" \
  --remote-adapter "$TMP/bin/remote-adapter" --json > "$TMP/remote.json"
python3 - "$TMP/remote.json" <<'PY' || fail "remote check is invalid"
import json, sys
d = json.load(open(sys.argv[1], encoding="utf-8"))
assert d["ok"] is True and d["profile"] == "remote", d
assert d["isolation"] == "external", d
assert d["requirements"]["adapter_executable"] is True, d
assert d["remote_adapter"], d
PY

# The compatibility preflight and the typed execution backend must resolve one
# container engine. Before the shared resolver, only execution-profile honored
# OMS_DOCKER_BIN while runtime silently selected docker/podman from PATH.
shared_engine="$TMP/bin/shared-engine"
shared_log="$TMP/shared-engine.argv"
cat > "$shared_engine" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$OMS_SHARED_ENGINE_LOG"
case "${1:-}" in
  info) exit 0 ;;
  image) [ "${2:-}" = inspect ] && exit 0 ;;
esac
exit 9
EOF
chmod +x "$shared_engine"

OMS_DOCKER_BIN="$shared_engine" OMS_SHARED_ENGINE_LOG="$shared_log" \
  "$PROFILE" check --profile isolated --repo "$ROOT" --image oms/shared:latest --json \
  > "$TMP/shared-compat.json" || fail "compatibility preflight rejected the shared engine"
OMS_DOCKER_BIN="$shared_engine" OMS_SHARED_ENGINE_LOG="$shared_log" \
  "$ROOT/scripts/runtime.sh" --repo "$ROOT" backend check isolated \
    --image oms/shared:latest > "$TMP/shared-runtime.json" ||
  fail "runtime backend rejected the shared engine"
if ! python3 - "$shared_engine" "$TMP/shared-runtime.json" <<'PY'
import json, os, sys
row = json.load(open(sys.argv[2], encoding="utf-8"))
assert row["ready"] is True, row
assert row["details"]["engine"] == os.path.basename(sys.argv[1]), row
assert "engine_path" not in row["details"], row
PY
then
  fail "runtime backend did not report the shared engine identity"
fi
[ "$(wc -l < "$shared_log" | tr -d ' ')" -eq 4 ] ||
  fail "the two front doors did not probe one shared engine exactly twice each"

echo "execution-profile-smoke: ok"
