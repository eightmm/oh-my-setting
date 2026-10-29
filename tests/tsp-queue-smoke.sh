#!/usr/bin/env bash
set -euo pipefail

# The fallback queue's monitor process dies as soon as its job finishes and
# its status file is written. Under a deferring Linux child-subreaper — the
# posture autopilot-receipt supervise holds around every acceptance phase —
# that corpse stays an unreaped zombie, and a kill -0 liveness probe keeps
# calling it alive: `wait` then polls forever and `list` reports "running".
# Gone or zombie is finished; only a runnable monitor is a live job.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
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

repo="$TMP/repo with spaces"
mkdir -p "$repo"
git -C "$repo" init -q
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
printf 'base\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base
cd "$repo"

# Enqueue attribution is data, shared by real queues and the fallback reader.
export OMS_TSP_STATE_DIR="$TMP/queue"
python3 - "$TSP" "$repo" "$TMP" <<'PY'
import json, os, pathlib, subprocess, sys
tsp, repo, scratch = sys.argv[1:]
root = pathlib.Path(scratch)
alias = root / "repo alias"
try:
    alias.symlink_to(repo, target_is_directory=True)
except (OSError, NotImplementedError):
    alias = pathlib.Path(repo)
binary = root / "bin"
binary.mkdir()
stub = binary / "tsp"
stub.write_text('#!/usr/bin/env bash\ncase "$1" in -S) exit 0;; *) printf "%s\\n" "${TSP_FIXTURE_JOB-42}";; esac\n')
stub.chmod(0o755)
env = dict(os.environ, PATH=str(binary) + os.pathsep + os.environ["PATH"], TS_SOCKET=str(root / "socket"))
label = "attributed job"
for fallback in (False, True):
    env["OMS_TSP_FORCE_FALLBACK"] = "1" if fallback else "0"
    result = subprocess.run([tsp, "enqueue", "--label", label, "--", "true"],
                            cwd=alias, env=env, capture_output=True, text=True, timeout=20)
    assert result.returncode == 0, result.stderr
    job = result.stdout.strip()
    assert job.isdigit(), result.stdout
    directory = pathlib.Path(env["OMS_TSP_FALLBACK_DIR"] if fallback else env["OMS_TSP_STATE_DIR"])
    path = directory / (job + ".meta" if fallback else job)
    lines = path.read_text().splitlines()
    record = json.loads(next(line[len("# oms-panel "):] for line in lines if line.startswith("# oms-panel ")))
    assert record["project"] == str(pathlib.Path(repo).resolve()), record
    assert record["label"] == label, record
    if not fallback:
        assert record["queue_socket"] == env["TS_SOCKET"] and record["queue_command"] == "[attributed job]true", record
        assert result.stderr == "", result.stderr
    assert result.stdout == job + "\n", result.stdout
env["OMS_TSP_FORCE_FALLBACK"] = "0"
env.pop("TS_SOCKET")
env["TMPDIR"] = str(root)
result = subprocess.run([tsp, "enqueue", "--", "printf", "%s", "two words"], cwd=alias, env=env,
                        capture_output=True, text=True, timeout=20)
assert result.returncode == 0 and result.stdout == "42\n" and result.stderr == "", result
row = json.loads((pathlib.Path(env["OMS_TSP_STATE_DIR"]) / "42").read_text().splitlines()[0][len("# oms-panel "):])
assert row["queue_socket"] == str(root) + "/socket-ts." + subprocess.check_output(["id", "-u"], text=True).strip(), row
assert row["queue_command"] == "printf %s two words", row
env["TMPDIR"] = ""
result = subprocess.run([tsp, "enqueue", "--", "true"], cwd=alias, env=env,
                        capture_output=True, text=True, timeout=20)
assert result.returncode == 0, result.stderr
row = json.loads((pathlib.Path(env["OMS_TSP_STATE_DIR"]) / "42").read_text().splitlines()[0][len("# oms-panel "):])
assert row["queue_socket"] == "/socket-ts." + subprocess.check_output(["id", "-u"], text=True).strip(), row
for job, code in (("43\r", 0), ("0", 0), ("", 2), ("bad", 2)):
    env["TSP_FIXTURE_JOB"] = job
    result = subprocess.run([tsp, "enqueue", "--", "true"], cwd=root, env=env,
                            capture_output=True, text=True, timeout=20)
    assert result.returncode == code, (job, result.stderr)
    if code == 0:
        lines = (pathlib.Path(env["OMS_TSP_STATE_DIR"]) / job.strip()).read_text().splitlines()
        row = json.loads(next(line[len("# oms-panel "):] for line in lines if line.startswith("# oms-panel ")))
        assert row["project"] == "" and row["label"] == "", row
        assert row["queue_command"] == "true", row
PY

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
