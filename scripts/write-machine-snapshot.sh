#!/usr/bin/env bash
set -euo pipefail

# Write local/machine.md — a hardware/tooling snapshot agents read for local context.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${OH_MY_SETTING_MACHINE_SNAPSHOT:-$ROOT/local/machine.md}"
HOST_LABEL="${OH_MY_SETTING_HOST_LABEL:-local}"
MODE="write"

usage() {
  cat <<'EOF'
Usage: write-machine-snapshot.sh [--check|--dry-run] [-h|--help]

Write a private, atomic local hardware/tooling snapshot. --dry-run prints the
snapshot without writing it; --check validates the existing snapshot contract.
EOF
}

[ "$#" -le 1 ] || { usage >&2; exit 2; }
case "${1:-}" in
  "") ;;
  --check) MODE=check ;;
  --dry-run) MODE=dry-run ;;
  -h|--help) usage; exit 0 ;;
  *) echo "error: unknown option: $1" >&2; usage >&2; exit 2 ;;
esac

if [ "$MODE" = check ]; then
  [ -f "$OUT" ] || { echo "error: machine snapshot missing: $OUT" >&2; exit 1; }
  grep -Fqx -- '- Schema: 1' "$OUT" &&
    grep -Eq '^- Updated: [0-9]{4}-[0-9]{2}-[0-9]{2}T' "$OUT" &&
    grep -Fq '## Local Agent CLI Paths' "$OUT" || {
      echo "error: invalid machine snapshot: $OUT" >&2
      exit 1
    }
  echo "machine snapshot: ok ($OUT)"
  exit 0
fi

first_line() {
  "$@" 2>/dev/null | sed -n '1p'
}

system_summary() {
  python3 - "$HOST_LABEL" <<'PY'
import os
import platform
import shutil
import subprocess
import sys


def text(value):
    return str(value or "unknown").replace("\r", " ").replace("\n", " ").strip()


def total_memory():
    try:
        return os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
    except (AttributeError, OSError, TypeError, ValueError):
        return None


def read_file(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return handle.read()
    except OSError:
        return ""


# platform.platform() answers "Linux-6.8.0-x86_64-with-glibc2.39" — the kernel
# line again, with the distribution gone. The reason this snapshot exists is to
# tell an agent what machine it is on, so the distro name comes first and the
# portable form is the fallback, not the answer.
def os_name():
    for line in read_file(OS_RELEASE).splitlines():
        if line.startswith("PRETTY_NAME="):
            return line.split("=", 1)[1].strip().strip('"')
    if platform.system() == "Darwin":
        release = platform.mac_ver()[0]
        if release:
            return f"macOS {release}"
    return platform.platform()


# platform.processor() returns the architecture on Linux ("x86_64") and "arm"
# on Apple silicon. Neither identifies the part, which is the whole question a
# CPU line is asked.
def cpu_name():
    for line in read_file(CPUINFO).splitlines():
        if line.startswith("model name"):
            return line.split(":", 1)[1].strip()
    if shutil.which("sysctl"):
        try:
            brand = subprocess.run(
                ["sysctl", "-n", "machdep.cpu.brand_string"],
                capture_output=True, text=True, timeout=5,
            ).stdout.strip()
        except (OSError, subprocess.SubprocessError):
            brand = ""
        if brand:
            return brand
    return (
        os.environ.get("PROCESSOR_IDENTIFIER")
        or platform.processor()
        or platform.machine()
    )


# Overridable so the regression can feed known fixtures instead of asserting
# against whatever machine the suite happens to run on.
OS_RELEASE = os.environ.get("OMS_OS_RELEASE", "/etc/os-release")
CPUINFO = os.environ.get("OMS_CPUINFO", "/proc/cpuinfo")

memory = total_memory()
try:
    storage = shutil.disk_usage(os.path.expanduser("~"))
    storage_text = (
        f"{storage.total / 1024 ** 3:.1f} GiB total, "
        f"{storage.free / 1024 ** 3:.1f} GiB available"
    )
except OSError:
    storage_text = "unknown"

print(f"- Host label: {text(sys.argv[1])}")
print(f"- OS: {text(os_name())}")
print(f"- Kernel: {text(platform.release())}")
print(f"- CPU: {text(cpu_name())}")
print(f"- CPU cores: {os.cpu_count() or 'unknown'}")
print(f"- RAM: {memory / 1024 ** 3:.1f} GiB" if memory else "- RAM: unknown")
print(f"- Home storage: {storage_text}")
PY
}

gpu_summary() {
  local output=""

  if command -v nvidia-smi >/dev/null 2>&1; then
    if output="$(nvidia-smi --query-gpu=name,memory.total --format=csv,noheader 2>/dev/null)" &&
       [ -n "$output" ]; then
      printf '%s\n' "$output" | sort | uniq -c | sed 's/^ *//'
    else
      printf 'none detected\n'
    fi
  elif command -v rocm-smi >/dev/null 2>&1; then
    output="$(rocm-smi --showproductname --showmeminfo vram 2>/dev/null || true)"
    if [ -n "$output" ]; then
      printf '%s\n' "$output" | sed -n '1,12p'
    else
      printf 'none detected\n'
    fi
  elif [ "$(uname -s)" = Darwin ] && command -v system_profiler >/dev/null 2>&1; then
    output="$(system_profiler SPDisplaysDataType 2>/dev/null || true)"
    printf '%s\n' "$output" |
      sed -n 's/^[[:space:]]*Chipset Model: /chipset: /p'
  else
    printf 'none detected\n'
  fi
}

nvidia_driver() {
  local value=""

  if command -v nvidia-smi >/dev/null 2>&1; then
    value="$(nvidia-smi --query-gpu=driver_version --format=csv,noheader 2>/dev/null)" ||
      value=""
  fi
  if [ -n "$value" ]; then
    printf '%s\n' "$value" | sed -n '1p'
  else
    printf 'n/a\n'
  fi
}

cuda_version() {
  local parsed=""
  local value=""

  if command -v nvcc >/dev/null 2>&1; then
    value="$(nvcc --version 2>/dev/null || true)"
    parsed="$(printf '%s\n' "$value" |
      sed -n 's/.*release \([^,]*\).*/\1/p' | sed -n '1p')"
  elif command -v nvidia-smi >/dev/null 2>&1; then
    value="$(nvidia-smi 2>/dev/null)" || value=""
    parsed="$(printf '%s\n' "$value" |
      sed -n 's/.*CUDA Version: \([^ ]*\).*/\1/p' | sed -n '1p')"
  fi
  if [ -n "$parsed" ]; then printf '%s\n' "$parsed"; else printf 'n/a\n'; fi
}

slurm_status() {
  if command -v sinfo >/dev/null 2>&1; then
    printf 'available\n'
  else
    printf 'not detected\n'
  fi
}

tsp_status() {
  local slots
  if command -v tsp >/dev/null 2>&1; then
    slots="$(tsp -S 2>/dev/null | sed -n 1p | tr -d '[:space:]')" || slots=""
    case "$slots" in
      ''|*[!0-9]*) printf 'available\n' ;;
      *) printf 'available (%s slot(s))\n' "$slots" ;;
    esac
  else
    printf 'not detected\n'
  fi
}

tool_path() {
  local name="$1"
  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
  else
    printf 'not detected\n'
  fi
}

render_snapshot() {
  printf '# Machine Snapshot\n\n'
  printf 'Compact local compute snapshot. Do not commit if it contains private details.\n\n'
  printf -- '- Schema: 1\n'
  printf -- '- Updated: %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  system_summary
  printf -- '- GPU:\n'
  gpu_summary | sed 's/^/  - /'
  printf -- '- NVIDIA driver: %s\n' "$(nvidia_driver)"
  printf -- '- CUDA: %s\n' "$(cuda_version)"
  printf -- '- Python: %s\n' "$(first_line python3 --version || printf 'not detected')"
  printf -- '- uv: %s\n' "$(first_line uv --version || printf 'not detected')"
  printf -- '- Slurm: %s\n' "$(slurm_status)"
  printf -- '- tsp queue: %s\n' "$(tsp_status)"
  printf '\n## Local Agent CLI Paths\n\n'
  printf -- '- Codex CLI: %s\n' "$(tool_path codex)"
  printf -- '- Claude Code CLI: %s\n' "$(tool_path claude)"
  printf -- '- Antigravity CLI: %s\n' "$(tool_path agy)"
  printf -- '- gh: %s\n' "$(tool_path gh)"
  printf '\n## Notes\n\n'
  printf -- '- Project envs should stay local at `.venv` and run through `uv`.\n'
  printf -- '- Keep private usernames, accounts, tokens, and mount paths out of this file.\n'
}

if [ "$MODE" = dry-run ]; then
  render_snapshot
  exit 0
fi

mkdir -p "$(dirname "$OUT")"
tmp="$(mktemp "$(dirname "$OUT")/.machine.XXXXXX")" || exit 1
trap 'rm -f "$tmp"' EXIT HUP INT TERM
render_snapshot > "$tmp"
chmod 600 "$tmp"
mv -f "$tmp" "$OUT"
trap - EXIT HUP INT TERM

echo "wrote $OUT"
