#!/usr/bin/env bash
set -euo pipefail

# The fallback queue's monitor process dies as soon as its job finishes and
# its status file is written. Under a deferring Linux child-subreaper — the
# posture autopilot-receipt supervise holds around every acceptance phase —
# that corpse stays an unreaped zombie, and a kill -0 liveness probe keeps
# calling it alive: `wait` then polls forever and `list` reports "running".
# Gone or zombie is finished; only a runnable monitor is a live job.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TSP="$ROOT/scripts/tsp-queue.sh"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-tsp-queue.XXXXXX")"
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() { echo "tsp-queue-smoke: $*" >&2; exit 1; }

export HOME="$TMP/home"
export XDG_STATE_HOME="$TMP/state"
export OMS_LOCK_DIR="$TMP/locks"
export OMS_TSP_FORCE_FALLBACK=1
export OMS_TSP_ALLOW_FALLBACK=1
export OMS_TSP_FALLBACK_DIR="$TMP/fb"
mkdir -p "$HOME" "$XDG_STATE_HOME" "$OMS_LOCK_DIR"

repo="$TMP/repo"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
printf 'base\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base
cd "$repo"

if [ "$(uname -s)" = Linux ]; then
  python3 - "$TSP" "$OMS_TSP_FALLBACK_DIR" <<'PY' || fail "wait or list misjudged a subreaper-held zombie monitor"
import ctypes, glob, os, subprocess, sys, time

try:
    libc = ctypes.CDLL(None, use_errno=True)
    libc.prctl(36, 1, 0, 0, 0)  # PR_SET_CHILD_SUBREAPER; best effort
except (OSError, AttributeError):
    pass
tsp, fb_dir = sys.argv[1:3]

job = subprocess.run([tsp, "enqueue", "--allow-noqueue", "--", "true"],
                     stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                     check=True, text=True).stdout.strip().splitlines()[-1]
assert job.isdigit(), job

# The job is trivial: its status file appears almost at once, after which the
# monitor is dead — held as our unreaped zombie until this process exits.
deadline = time.monotonic() + 10
while time.monotonic() < deadline:
    if glob.glob(os.path.join(fb_dir, "*.status")):
        break
    time.sleep(0.05)
else:
    print("fallback status file never appeared", file=sys.stderr)
    sys.exit(1)

start = time.monotonic()
try:
    result = subprocess.run([tsp, "wait", job], stdout=subprocess.PIPE,
                            stderr=subprocess.DEVNULL, timeout=25, text=True)
except subprocess.TimeoutExpired:
    print("wait polled a zombie monitor past the timeout", file=sys.stderr)
    sys.exit(1)
elapsed = time.monotonic() - start
assert result.returncode == 0, result.returncode
assert result.stdout.strip().splitlines()[-1] == "0", result.stdout
assert elapsed < 20, elapsed

listing = subprocess.run([tsp, "list"], stdout=subprocess.PIPE,
                         stderr=subprocess.DEVNULL, check=True, text=True).stdout
row = next((line for line in listing.splitlines() if line.startswith(job + "\t")), "")
assert row, listing
assert "running" not in row, row
assert "exit 0" in row, row
PY
fi

echo "tsp-queue-smoke: ok"
