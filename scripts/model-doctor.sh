#!/usr/bin/env bash
set -euo pipefail

# Diagnose provider CLI compatibility, configured model routes, and quorum diversity.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/lib/model-routing.sh
. "$ROOT/scripts/lib/model-routing.sh"
# shellcheck source=scripts/lib/provider-registry.sh
. "$ROOT/scripts/lib/provider-registry.sh"

OUTPUT=json-disabled
LIVE_MODELS=0
REQUIRE_ALL=0
PROVIDERS="codex,claude,antigravity"
TIMEOUT_SECONDS="${OMS_MODEL_DOCTOR_TIMEOUT_SECONDS:-20}"

usage() {
  cat <<'USAGE'
Usage: model-doctor.sh [options]

Inspect the installed provider CLIs and the model routes selected by the harness.
The default check is local-only. --live-models additionally asks providers with
an official model-list command to verify the configured names against the
current account-visible catalog.

Options:
  --json              Emit one schema-versioned JSON document.
  --live-models       Run bounded live model-list probes where supported.
  --require-all       Treat a missing codex, claude, or agy binary as failure.
  --providers CSV     Provider subset (codex, claude, antigravity/agy).
  --timeout SECONDS   Per-command timeout, 1..300 (default: 20).
  -h, --help          Show this help.
USAGE
}

fail_usage() {
  echo "error: $*" >&2
  usage >&2
  exit 2
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --json)
      OUTPUT=json
      shift
      ;;
    --live-models)
      LIVE_MODELS=1
      shift
      ;;
    --require-all)
      REQUIRE_ALL=1
      shift
      ;;
    --providers)
      [ "$#" -ge 2 ] || fail_usage "--providers requires CSV"
      PROVIDERS="$2"
      shift 2
      ;;
    --timeout)
      [ "$#" -ge 2 ] || fail_usage "--timeout requires seconds"
      TIMEOUT_SECONDS="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      fail_usage "unknown argument: $1"
      ;;
  esac
done

case "$TIMEOUT_SECONDS" in
  ''|*[!0-9]*) fail_usage "--timeout must be an integer in 1..300" ;;
esac
[ "$TIMEOUT_SECONDS" -ge 1 ] && [ "$TIMEOUT_SECONDS" -le 300 ] ||
  fail_usage "--timeout must be an integer in 1..300"
PROVIDERS="$(oms_provider_normalize_list "$PROVIDERS")" || exit $?

command -v python3 >/dev/null 2>&1 || {
  echo "error: python3 is required for bounded provider probes and JSON encoding" >&2
  exit 1
}

export PATH="$HOME/.local/bin:$PATH"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-model-doctor.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
ROWS="$TMP/providers.jsonl"
: > "$ROWS"

run_capture() {
  local seconds="$1"
  local output_file="$2"
  shift 2

  python3 - "$seconds" "$output_file" "$@" <<'PY'
import os
import signal
import subprocess
import sys

seconds = int(sys.argv[1])
path = sys.argv[2]
command = sys.argv[3:]
try:
    with open(path, "wb") as output:
        process = subprocess.Popen(
            command,
            stdout=output,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            returncode = process.wait(timeout=seconds)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
            output.write(("\nmodel-doctor: command timed out after %ss\n" % seconds).encode())
            raise SystemExit(124)
except OSError as exc:
    with open(path, "ab") as output:
        output.write(("model-doctor: command failed to start: %s\n" % exc).encode())
    raise SystemExit(127)
raise SystemExit(returncode if returncode <= 125 else 1)
PY
}

append_row() {
  local provider="$1"
  local binary="$2"
  local installed="$3"
  local version="$4"
  local version_exit="$5"
  local help_exit="$6"
  local required_file="$7"
  local missing_file="$8"
  local routes_file="$9"
  local listing_supported="${10}"
  local listing_status="${11}"
  local listing_exit="${12}"
  local listing_output="${13}"

  python3 - "$ROWS" "$provider" "$binary" "$installed" "$version" \
    "$version_exit" "$help_exit" "$required_file" "$missing_file" \
    "$routes_file" "$listing_supported" "$listing_status" "$listing_exit" \
    "$listing_output" <<'PY'
import json
import sys

(
    rows_path,
    provider,
    binary,
    installed,
    version,
    version_exit,
    help_exit,
    required_path,
    missing_path,
    routes_path,
    listing_supported,
    listing_status,
    listing_exit,
    listing_output,
) = sys.argv[1:]

def lines(path):
    with open(path, encoding="utf-8") as handle:
        return [line.rstrip("\n") for line in handle if line.rstrip("\n")]

routes = {}
with open(routes_path, encoding="utf-8") as handle:
    for raw in handle:
        model_class, model, family, effort = raw.rstrip("\n").split("\t", 3)
        routes[model_class] = {
            "model": model,
            "family": family,
            "reasoning_effort": effort or None,
            "availability": "unverified",
        }

row = {
    "provider": provider,
    "binary": binary,
    "installed": installed == "1",
    "version": version or None,
    "version_exit": int(version_exit),
    "help_exit": int(help_exit),
    "required_flags": lines(required_path),
    "missing_flags": lines(missing_path),
    "model_listing_supported": listing_supported == "1",
    "model_list_status": listing_status,
    "model_list_exit": int(listing_exit),
    "routes": routes,
    "_model_output_path": listing_output,
}
with open(rows_path, "a", encoding="utf-8") as handle:
    handle.write(json.dumps(row, ensure_ascii=False, allow_nan=False) + "\n")
PY
}

IFS=',' read -r -a provider_values <<< "$PROVIDERS"
for provider in "${provider_values[@]}"; do
  binary="$(oms_provider_binary "$provider")"
  provider_dir="$TMP/$provider"
  mkdir -p "$provider_dir"
  required_file="$provider_dir/required"
  missing_file="$provider_dir/missing"
  routes_file="$provider_dir/routes"
  version_file="$provider_dir/version"
  help_file="$provider_dir/help"
  models_file="$provider_dir/models"
  : > "$required_file"
  : > "$missing_file"
  : > "$routes_file"
  : > "$version_file"
  : > "$help_file"
  : > "$models_file"

  oms_provider_required_flags "$provider" > "$required_file"
  installed=0
  version=""
  version_exit=127
  help_exit=127
  listing_supported=0
  listing_status=not-requested
  listing_exit=0

  if command -v "$binary" >/dev/null 2>&1; then
    installed=1
    version_exit=0
    run_capture 5 "$version_file" "$binary" --version || version_exit=$?
    version="$(head -n 1 "$version_file" | tr '\t\r\n' '   ' | cut -c1-200)"

    help_exit=0
    case "$provider" in
      codex) run_capture 5 "$help_file" "$binary" exec --help || help_exit=$? ;;
      *) run_capture 5 "$help_file" "$binary" --help || help_exit=$? ;;
    esac
    if [ "$help_exit" -eq 0 ]; then
      while IFS= read -r flag; do
        [ -n "$flag" ] || continue
        grep -Fq -- "$flag" "$help_file" || printf '%s\n' "$flag" >> "$missing_file"
      done < "$required_file"
    fi

    if oms_provider_supports_model_listing "$provider"; then
      listing_supported=1
      if [ "$LIVE_MODELS" -eq 1 ]; then
        listing_exit=0
        run_capture "$TIMEOUT_SECONDS" "$models_file" "$binary" models || listing_exit=$?
        if [ "$listing_exit" -eq 0 ]; then
          listing_status=ok
        else
          listing_status=failed
        fi
      fi
    elif [ "$LIVE_MODELS" -eq 1 ]; then
      listing_status=unsupported
    fi
  else
    listing_status=not-installed
  fi

  for model_class in fast balanced deep; do
    model="$(oms_model_mapping "$provider" "$model_class")"
    oms_model_validate_name "$model" || exit $?
    family="$(oms_provider_model_family "$provider" "$model")"
    effort="$(oms_reasoning_for_class "$model_class")"
    if [ "$provider" = antigravity ]; then
      effort="$(oms_reasoning_from_model "$model")"
    fi
    printf '%s\t%s\t%s\t%s\n' "$model_class" "$model" "$family" "$effort" >> "$routes_file"
  done

  append_row "$provider" "$binary" "$installed" "$version" "$version_exit" \
    "$help_exit" "$required_file" "$missing_file" "$routes_file" \
    "$listing_supported" "$listing_status" "$listing_exit" "$models_file"
done

python3 - "$ROWS" "$OUTPUT" "$LIVE_MODELS" "$REQUIRE_ALL" <<'PY'
import json
import re
import sys
from collections import defaultdict

rows_path, output_mode, live_raw, require_raw = sys.argv[1:]
live = live_raw == "1"
require_all = require_raw == "1"
with open(rows_path, encoding="utf-8") as handle:
    providers = [json.loads(line) for line in handle if line.strip()]

ansi = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")

def model_lines(path):
    if not path:
        return []
    try:
        text = open(path, encoding="utf-8", errors="replace").read()
    except OSError:
        return []
    values = []
    for raw in text.splitlines():
        line = ansi.sub("", raw).strip()
        line = re.sub(r"^(?:[-*•]|\d+[.)])\s+", "", line).strip()
        if line:
            values.append(line)
    return values

warnings = []
errors = []
for provider in providers:
    name = provider["provider"]
    if not provider["installed"]:
        message = f"{name}: provider binary '{provider['binary']}' is not installed"
        (errors if require_all else warnings).append(message)
        for route in provider["routes"].values():
            route["availability"] = "not-checked"
        continue

    if provider["version_exit"] != 0:
        warnings.append(f"{name}: version probe failed with exit {provider['version_exit']}")
    if provider["help_exit"] != 0:
        errors.append(f"{name}: CLI help probe failed with exit {provider['help_exit']}")
    if provider["missing_flags"]:
        errors.append(
            f"{name}: installed CLI is missing required flags: "
            + ", ".join(provider["missing_flags"])
        )

    if not live:
        for route in provider["routes"].values():
            route["availability"] = "unverified"
        continue

    if not provider["model_listing_supported"]:
        warnings.append(
            f"{name}: no stable model-list probe is registered; configured models remain unverified"
        )
        for route in provider["routes"].values():
            route["availability"] = "unverified"
        continue

    if provider["model_list_status"] != "ok":
        errors.append(
            f"{name}: live model-list probe failed with exit {provider['model_list_exit']}"
        )
        for route in provider["routes"].values():
            route["availability"] = "probe-failed"
        continue

    discovered = model_lines(provider.get("_model_output_path"))
    provider["discovered_model_count"] = len(discovered)
    for model_class, route in provider["routes"].items():
        model = route["model"]
        available = any(line == model or model in line for line in discovered)
        route["availability"] = "available" if available else "missing"
        if not available:
            errors.append(
                f"{name}: configured {model_class} model is not in the live catalog: {model}"
            )

# Provider identity alone is not an independence proof. Compare the underlying
# configured family for every tier, including models surfaced through agy.
diversity = []
for model_class in ("fast", "balanced", "deep"):
    participants = []
    by_family = defaultdict(list)
    unknown = []
    for provider in providers:
        route = provider["routes"][model_class]
        item = {
            "provider": provider["provider"],
            "model": route["model"],
            "family": route["family"],
        }
        participants.append(item)
        if route["family"] == "unknown":
            unknown.append(provider["provider"])
        else:
            by_family[route["family"]].append(provider["provider"])

    duplicate_groups = {
        family: names for family, names in by_family.items() if len(names) > 1
    }
    if len(participants) < 2:
        status = "insufficient"
        warnings.append(
            f"{model_class}: model-family diversity needs at least two providers"
        )
    elif duplicate_groups:
        status = "duplicate"
        for family, names in sorted(duplicate_groups.items()):
            errors.append(
                f"{model_class}: model-family quorum is not independent; "
                f"{family} is used by {', '.join(names)}"
            )
    elif unknown:
        status = "unknown"
        warnings.append(
            f"{model_class}: model-family independence cannot be proven for "
            + ", ".join(unknown)
        )
    else:
        status = "independent"
    diversity.append(
        {"model_class": model_class, "status": status, "participants": participants}
    )

for provider in providers:
    provider.pop("_model_output_path", None)

result = {
    "schema": 1,
    "ok": not errors,
    "live_models": live,
    "require_all": require_all,
    "providers": providers,
    "diversity": diversity,
    "warnings": warnings,
    "errors": errors,
}

if output_mode == "json":
    print(json.dumps(result, ensure_ascii=False, allow_nan=False, sort_keys=True))
else:
    print("# oh-my-setting model doctor\n")
    for provider in providers:
        installed = "installed" if provider["installed"] else "missing"
        version = provider.get("version") or "unknown"
        print(f"## {provider['provider']} ({installed})")
        print(f"binary: {provider['binary']}")
        print(f"version: {version}")
        if provider["installed"] and provider["help_exit"] == 0 and not provider["missing_flags"]:
            print("capabilities: ok")
        elif provider["installed"]:
            missing = ", ".join(provider["missing_flags"]) or f"help exit {provider['help_exit']}"
            print(f"capabilities: incompatible ({missing})")
        else:
            print("capabilities: not checked")
        for model_class in ("fast", "balanced", "deep"):
            route = provider["routes"][model_class]
            effort = route.get("reasoning_effort") or "model-embedded/unknown"
            print(
                f"{model_class}: {route['model']} "
                f"[family={route['family']}, effort={effort}, "
                f"availability={route['availability']}]"
            )
        print()

    print("## configured quorum diversity")
    for item in diversity:
        families = ", ".join(
            f"{p['provider']}={p['family']}" for p in item["participants"]
        )
        print(f"{item['model_class']}: {item['status']} ({families})")

    if warnings:
        print("\n## warnings")
        for message in warnings:
            print(f"- {message}")
    if errors:
        print("\n## errors")
        for message in errors:
            print(f"- {message}")
    print("\nmodel-doctor: " + ("ok" if not errors else "failed"))

raise SystemExit(0 if not errors else 1)
PY
