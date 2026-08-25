#!/usr/bin/env bash
set -euo pipefail

# Describe and validate execution boundaries. This command performs probes only:
# it never installs an engine, pulls images, connects to a remote, or launches work.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"

ACTION=""
PROFILE=""
REPO="${OMS_STATE_REPO:-$PWD}"
AS_JSON=0
IMAGE="${OMS_ISOLATED_IMAGE:-}"
REMOTE_ADAPTER="${OMS_REMOTE_ADAPTER:-}"

usage() {
  cat <<'EOF'
Usage: execution-profile.sh <command> [options]

Commands:
  list       Print the supported profile names.
  describe   Explain a profile and its environment requirements.
  check      Validate that the selected profile can be used on this machine.

Options:
  --profile NAME          trusted-local, isolated, or remote.
  --repo PATH             Repository checked for local access. Default: PWD.
  --image IMAGE           Container image for isolated; or OMS_ISOLATED_IMAGE.
  --remote-adapter PATH   Executable remote adapter; or OMS_REMOTE_ADAPTER.
  --json                  Print machine-readable JSON.

Profiles:
  trusted-local  Runs as the current host user with inherited filesystem,
                 credentials, and network access. It provides no sandbox.
  isolated       Requires a working Docker- or Podman-compatible engine plus
                 an explicit image. This command never installs an engine or
                 pulls the image.
  remote         Requires an explicit executable adapter. The adapter owns
                 authentication, transport, isolation, and remote cleanup.
EOF
}

fail() { echo "execution-profile: $*" >&2; exit 2; }

while [ "$#" -gt 0 ]; do
  case "$1" in
    list|describe|check)
      [ -z "$ACTION" ] || fail "multiple commands: $ACTION, $1"
      ACTION="$1"; shift ;;
    --profile) [ "$#" -ge 2 ] || fail "--profile requires a value"; PROFILE="$2"; shift 2 ;;
    --repo) [ "$#" -ge 2 ] || fail "--repo requires a path"; REPO="$2"; shift 2 ;;
    --image) [ "$#" -ge 2 ] || fail "--image requires a value"; IMAGE="$2"; shift 2 ;;
    --remote-adapter) [ "$#" -ge 2 ] || fail "--remote-adapter requires a path"; REMOTE_ADAPTER="$2"; shift 2 ;;
    --json) AS_JSON=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown argument: $1" ;;
  esac
done

[ -n "$ACTION" ] || { usage >&2; exit 2; }

if [ "$ACTION" = list ]; then
  if [ "$AS_JSON" -eq 1 ]; then
    printf '["trusted-local","isolated","remote"]\n'
  else
    printf 'trusted-local\nisolated\nremote\n'
  fi
  exit 0
fi

[ -n "$PROFILE" ] || fail "$ACTION requires --profile"
case "$PROFILE" in
  trusted-local|isolated|remote) ;;
  *) fail "unsupported profile: $PROFILE (expected trusted-local, isolated, or remote)" ;;
esac

profile_isolation() {
  case "$PROFILE" in
    trusted-local) printf 'none\n' ;;
    isolated) printf 'docker\n' ;;
    remote) printf 'external\n' ;;
  esac
}

profile_description() {
  case "$PROFILE" in
    trusted-local)
      printf '%s\n' 'Runs directly as the current host user. Filesystem, credentials, processes, and network are inherited; this is not a sandbox.' ;;
    isolated)
      printf '%s\n' 'Runs through an explicitly configured container image. Engine installation, image pulling, mounts, credentials, and network policy remain operator-owned.' ;;
    remote)
      printf '%s\n' 'Runs through an external executable adapter. That adapter owns authentication, transport, remote isolation, cancellation, and cleanup.' ;;
  esac
}

profile_environment_json() {
  case "$PROFILE" in
    trusted-local) printf '[]' ;;
    isolated)
      printf '[{"name":"OMS_ISOLATED_IMAGE","required":true,"alternative":"--image"},{"name":"OMS_DOCKER_BIN","required":false,"default":"docker"}]' ;;
    remote)
      printf '[{"name":"OMS_REMOTE_ADAPTER","required":true,"alternative":"--remote-adapter"}]' ;;
  esac
}

profile_commands_json() {
  case "$PROFILE" in
    trusted-local|remote) printf '["bash","git","python3"]' ;;
    isolated) printf '["bash","git","python3","docker"]' ;;
  esac
}

profile_constraints_json() {
  case "$PROFILE" in
    trusted-local)
      printf '{"host_credentials_inherited":true,"host_network_inherited":true,"sandbox":false}' ;;
    isolated)
      printf '{"local_image_required":true,"mount_policy_operator_owned":true,"network_policy_operator_owned":true}' ;;
    remote)
      printf '{"adapter_owns_authentication":true,"adapter_owns_transport":true,"adapter_owns_cleanup":true}' ;;
  esac
}

emit_description_json() {
  OMS_EP_PROFILE="$PROFILE" \
  OMS_EP_ISOLATION="$(profile_isolation)" \
  OMS_EP_DESCRIPTION="$(profile_description)" \
  OMS_EP_ENVIRONMENT="$(profile_environment_json)" \
  OMS_EP_COMMANDS="$(profile_commands_json)" \
  OMS_EP_CONSTRAINTS="$(profile_constraints_json)" \
  python3 <<'PY'
import json, os
print(json.dumps({
    "schema": 1,
    "profile": os.environ["OMS_EP_PROFILE"],
    "description": os.environ["OMS_EP_DESCRIPTION"],
    "isolation": os.environ["OMS_EP_ISOLATION"],
    "automatic_install": False,
    "constraints": json.loads(os.environ["OMS_EP_CONSTRAINTS"]),
    "requirements": {
        "commands": json.loads(os.environ["OMS_EP_COMMANDS"]),
        "environment": json.loads(os.environ["OMS_EP_ENVIRONMENT"]),
    },
}, sort_keys=True))
PY
}

if [ "$ACTION" = describe ]; then
  if [ "$AS_JSON" -eq 1 ]; then
    command -v python3 >/dev/null 2>&1 || fail "python3 is required for --json"
    emit_description_json
  else
    printf 'profile: %s\n' "$PROFILE"
    printf 'isolation: %s\n' "$(profile_isolation)"
    printf 'automatic install: no\n'
    printf 'description: %s\n' "$(profile_description)"
    case "$PROFILE" in
      trusted-local) printf 'requirements: bash, git, python3\n' ;;
      isolated) printf 'requirements: bash, git, python3, Docker or Podman, a local --image or OMS_ISOLATED_IMAGE\n' ;;
      remote) printf 'requirements: bash, git, python3, --remote-adapter or OMS_REMOTE_ADAPTER\n' ;;
    esac
  fi
  exit 0
fi

missing=""
for required in bash git python3; do
  if ! command -v "$required" >/dev/null 2>&1; then
    missing="${missing:+$missing, }$required"
  fi
done
[ -z "$missing" ] || fail "$PROFILE profile is missing required command(s): $missing"
[ -d "$REPO" ] || fail "repository path is not a directory: $REPO"
REPO="$(cd "$REPO" && pwd -P)"
case "$REPO" in *$'\r') REPO="${REPO%$'\r'}" ;; esac
git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1 ||
  fail "repository is not a readable Git worktree: $REPO"

DOCKER_CLIENT=false
DOCKER_DAEMON=false
IMAGE_LOCAL=false
ADAPTER_EXECUTABLE=false
RESOLVED_REMOTE_ADAPTER=""
TYPED_CHECK=""

typed_backend_check() {
  PYTHONPATH="$ROOT/scripts/lib${PYTHONPATH:+:$PYTHONPATH}" \
    python3 - "$PROFILE" "$IMAGE" "$REMOTE_ADAPTER" <<'PY'
import json
import sys

from oms_runtime.execution import check

print(json.dumps(check(sys.argv[1], image=sys.argv[2], adapter=sys.argv[3],
                       _include_resolved_paths=True),
                 sort_keys=True))
PY
}

typed_value() {  # JSON dotted.path
  printf '%s\n' "$1" | python3 -c '
import json, sys
value = json.load(sys.stdin)
for key in sys.argv[1].split("."):
    value = value.get(key) if isinstance(value, dict) else None
if isinstance(value, bool):
    print("true" if value else "false")
elif value is not None:
    print(value)
' "$2"
}

if [ "$PROFILE" = isolated ]; then
  [ -n "$IMAGE" ] || fail "isolated profile requires --image or OMS_ISOLATED_IMAGE; no image is pulled automatically"
fi
if [ "$PROFILE" = remote ]; then
  [ -n "$REMOTE_ADAPTER" ] ||
    fail "remote profile requires an executable adapter via --remote-adapter or OMS_REMOTE_ADAPTER"
fi

TYPED_CHECK="$(typed_backend_check)" || fail "runtime backend readiness probe failed"
case "$TYPED_CHECK" in *$'\r') TYPED_CHECK="${TYPED_CHECK%$'\r'}" ;; esac

if [ "$PROFILE" = isolated ]; then
  engine_path="$(typed_value "$TYPED_CHECK" details.engine_path | tr -d '\r')"
  [ -n "$engine_path" ] ||
    fail "isolated profile requires Docker or Podman; install a container engine, then retry"
  DOCKER_CLIENT=true
  [ "$(typed_value "$TYPED_CHECK" details.daemon_ready | tr -d '\r')" = true ] ||
    fail "isolated profile requires a reachable Docker or Podman daemon; start it and retry"
  DOCKER_DAEMON=true
  if [ "$(typed_value "$TYPED_CHECK" details.image_local | tr -d '\r')" != true ]; then
    fail "isolated image is not available locally: $IMAGE; pull or build it explicitly, then retry"
  fi
  IMAGE_LOCAL=true
fi

if [ "$PROFILE" = remote ]; then
  RESOLVED_REMOTE_ADAPTER="$(typed_value "$TYPED_CHECK" details.adapter_path | tr -d '\r')"
  [ -n "$RESOLVED_REMOTE_ADAPTER" ] ||
    fail "remote profile requires an executable adapter: $REMOTE_ADAPTER"
  ADAPTER_EXECUTABLE=true
fi

if [ "$AS_JSON" -eq 1 ]; then
  OMS_EP_PROFILE="$PROFILE" OMS_EP_REPO="$REPO" \
  OMS_EP_ISOLATION="$(profile_isolation)" OMS_EP_IMAGE="$IMAGE" \
  OMS_EP_DOCKER_CLIENT="$DOCKER_CLIENT" OMS_EP_DOCKER_DAEMON="$DOCKER_DAEMON" \
  OMS_EP_IMAGE_LOCAL="$IMAGE_LOCAL" \
  OMS_EP_REMOTE_ADAPTER="$RESOLVED_REMOTE_ADAPTER" \
  OMS_EP_ADAPTER_EXECUTABLE="$ADAPTER_EXECUTABLE" \
  python3 <<'PY'
import json, os
print(json.dumps({
    "schema": 1,
    "ok": True,
    "profile": os.environ["OMS_EP_PROFILE"],
    "repo": os.environ["OMS_EP_REPO"],
    "isolation": os.environ["OMS_EP_ISOLATION"],
    "automatic_install": False,
    "image": os.environ["OMS_EP_IMAGE"],
    "remote_adapter": os.environ["OMS_EP_REMOTE_ADAPTER"],
    "requirements": {
        "commands": {"bash": True, "git": True, "python3": True},
        "docker_client": os.environ["OMS_EP_DOCKER_CLIENT"] == "true",
        "docker_daemon": os.environ["OMS_EP_DOCKER_DAEMON"] == "true",
        "image_local": os.environ["OMS_EP_IMAGE_LOCAL"] == "true",
        "adapter_executable": os.environ["OMS_EP_ADAPTER_EXECUTABLE"] == "true",
    },
}, sort_keys=True))
PY
else
  printf 'execution-profile: %s is ready (%s)\n' "$PROFILE" "$(profile_isolation)"
fi
