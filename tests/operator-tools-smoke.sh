#!/usr/bin/env bash
set -euo pipefail
umask 077

# Focused regressions for the read-only collaboration dashboard. Every fixture
# lives below TMP; no real provider is called.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# shellcheck source=tests/lib/smoke-env.sh
. "$ROOT/tests/lib/smoke-env.sh"
# Interactive fixtures allocate and answer their own private PTYs.

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

# A sandbox may deny AF_UNIX even when tmux and PTY support are installed.
# Probe the exact capability in TMP so workers can report partial coverage.
OPERATOR_SOCKET_DENIED=0
if python3 - <<'PY'
import errno
import os
import shutil
import socket
import sys
import tempfile
# A unique short directory: parallel shards must not collide on one name, and a
# long TMPDIR must not exceed the AF_UNIX path limit. Only a permission refusal
# means the sandbox denies sockets.
base = "/tmp" if os.path.isdir("/tmp") else None
directory = tempfile.mkdtemp(prefix="oms-sock-", dir=base)
probe = None
try:
    probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    probe.bind(os.path.join(directory, "p"))
except AttributeError:
    sys.exit(1)
except OSError as error:
    sys.exit(1 if error.errno in (errno.EPERM, errno.EACCES) else 0)
finally:
    if probe is not None:
        probe.close()
    shutil.rmtree(directory, ignore_errors=True)
PY
then
  :
else
  OPERATOR_SOCKET_DENIED=1
fi
export OMS_OPERATOR_SOCKET_DENIED="$OPERATOR_SOCKET_DENIED"

# Interactive fixtures allocate and answer their own private PTYs.
exec < /dev/null
TMP="$(mktemp -d "${TMPDIR:-/tmp}/oms-operator-tools.XXXXXX")"
operator_cleanup() {
  if [ -n "${OMS_OPERATOR_TIMINGS:-}" ] && [ -z "${OMS_OPERATOR_TIMINGS_DIR:-}" ] && [ -f "$OMS_OPERATOR_TIMINGS" ]; then
    while IFS=$'\t' read -r duration section; do
      [ -n "$section" ] && echo "operator-section: ${duration}s $section"
    done < <(sort -nr -k1,1 "$OMS_OPERATOR_TIMINGS")
  fi
  if [ "${OPERATOR_SPEC:-1/1}" != 1/1 ] && [ -n "${OMS_OPERATOR_TIMINGS_DIR:-}" ]; then
    printf '%s %s\n' "$OPERATOR_SPEC" "$((SECONDS - OPERATOR_SHARD_STARTED))" > "$OMS_OPERATOR_TIMINGS_DIR/operator-shard.${OPERATOR_SPEC%/*}"
  fi
  rm -rf "$TMP"
}
trap operator_cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
# Tier routing needs discoverable providers; fixtures never invoke real model CLIs.
mkdir -p "$TMP/provider-standins"
for provider in claude codex; do
  printf '#!/bin/sh\necho "%s stand-in: not a real CLI" >&2\nexit 1\n' "$provider" > "$TMP/provider-standins/$provider"
  chmod +x "$TMP/provider-standins/$provider"
done
PATH="$PATH:$TMP/provider-standins"
# In-process controllers also need private durable state, not the operator's home.
if [ -z "${OMS_FIXTURE_ROOT:-}" ]; then
  OMS_FIXTURE_ROOT="$TMP/fixture-root"
  (
    umask 077
    mkdir -p "$OMS_FIXTURE_ROOT/state"
  )
fi
oms_smoke_use_fixture_root
export OMS_LOCK_DIR="$TMP/locks"
# Panel snapshots must not read the host tsp queue or its fallback jobs.
export OMS_TSP_FORCE_FALLBACK=1 OMS_TSP_FALLBACK_DIR="$TMP/tsp-fallback"
mkdir -p "$OMS_LOCK_DIR"
OPERATOR_STARTED=$SECONDS
OPERATOR_SHARD_STARTED=$SECONDS
OPERATOR_RUNNER="$ROOT/tests/operator/runner.py"
OPERATOR_SPEC="${OMS_OPERATOR_SHARD:-1/1}"
OPERATOR_MODE=run
OPERATOR_SECTION=''
case "${1:-}" in
  --jobs)
    [ "$#" = 2 ] || fail '--jobs requires N'
    # shellcheck source=scripts/lib/check-parallel.sh
    . "$ROOT/scripts/lib/check-parallel.sh"
    python3 "$OPERATOR_RUNNER" --self-check "$2"
    OMS_OPERATOR_TIMINGS_DIR="$TMP"
    export OMS_OPERATOR_TIMINGS_DIR
    # The jobs summary replaces the ordinary cleanup trap in the parent.
    operator_timing_summary() {
      if [ -d "$OMS_OPERATOR_TIMINGS_DIR" ]; then
        while IFS=$'\t' read -r duration section; do
          [ -n "$section" ] && echo "operator-section: ${duration}s $section"
        done < <(cat "$OMS_OPERATOR_TIMINGS_DIR"/operator-timings.* 2>/dev/null | sort -nr -k1,1)
        for timing in "$OMS_OPERATOR_TIMINGS_DIR"/operator-shard.*; do
          [ -f "$timing" ] || continue
          read -r shard duration < "$timing"
          echo "operator-shard: $shard ${duration}s"
        done
      fi
      rm -rf "$TMP"
    }
    trap operator_timing_summary EXIT
    if [ "$OPERATOR_SOCKET_DENIED" = 1 ]; then
      OMS_OPERATOR_PARTIAL_MARKER_DIR="$TMP"
      export OMS_OPERATOR_PARTIAL_MARKER_DIR
      if OMS_OPERATOR_PROGRESS=1 oms_operator_parallel "${BASH_SOURCE[0]}" "$2" 3>&1; then
        parallel_status=0
      else
        parallel_status=$?
      fi
      partial_count=0
      for marker in "$TMP"/operator-partial.*; do
        [ -f "$marker" ] && partial_count=$((partial_count + 1))
      done
      if [ "$parallel_status" -ne 0 ] && [ "$partial_count" -eq "$2" ]; then
        exit 77
      fi
      exit "$parallel_status"
    fi
    # Shard logs are buffered by the shared runner; progress keeps its own stream.
    OMS_OPERATOR_PROGRESS=1 oms_operator_parallel "${BASH_SOURCE[0]}" "$2" 3>&1
    exit $? ;;
  --list|--self-check)
    [ "$#" -le 2 ] || fail 'expected --list [N] or --self-check [N]'
    exec_args=("$1" "${2:-4}")
    OPERATOR_MODE=list ;;
  --shard)
    [ "$#" = 2 ] || fail '--shard requires I/N'
    OPERATOR_SPEC="$2" ;;
  --list-sections)
    [ "$#" = 1 ] || fail '--list-sections takes no arguments'
    OPERATOR_MODE=sections-list ;;
  --section)
    [ "$#" = 2 ] || fail '--section requires NAME[,NAME...]'
    [ -z "${OMS_OPERATOR_SHARD:-}" ] || fail '--shard and --section are exclusive'
    OPERATOR_SECTION="$2" ;;
  -h|--help)
    echo 'usage: operator-tools-smoke.sh [--shard I/N | --section NAME[,NAME...] | --list-sections | --jobs N | --list [N] | --self-check [N]] (N=1..4)'
    echo 'OMS_OPERATOR_SHARD=I/N also selects a shard; no selector runs every section.'
    exit 0 ;;
  '') ;;
  *) fail "unknown argument: $1" ;;
esac
unset OMS_OPERATOR_SHARD
if [ "$OPERATOR_MODE" = list ]; then
  python3 "$OPERATOR_RUNNER" "${exec_args[@]}"
  exit $?
fi
if [ "$OPERATOR_MODE" = sections-list ]; then
  python3 "$OPERATOR_RUNNER" --list-sections
  exit $?
fi
if [ -n "${OMS_OPERATOR_TIMINGS_DIR:-}" ] && [ "$OPERATOR_SPEC" != 1/1 ]; then
  OMS_OPERATOR_TIMINGS="$OMS_OPERATOR_TIMINGS_DIR/operator-timings.${OPERATOR_SPEC%/*}"
  export OMS_OPERATOR_TIMINGS
fi
if [ -z "${OMS_OPERATOR_TIMINGS:-}" ]; then
  OMS_OPERATOR_TIMINGS="$TMP/operator-timings.single"
  export OMS_OPERATOR_TIMINGS
fi
operator_status=0
if [ -n "$OPERATOR_SECTION" ]; then
  PYTHONDONTWRITEBYTECODE=1 python3 "$OPERATOR_RUNNER" --run-sections "$TMP" "$OPERATOR_SECTION" || operator_status=$?
else
  PYTHONDONTWRITEBYTECODE=1 python3 "$OPERATOR_RUNNER" --run-shard "$TMP" "$OPERATOR_SPEC" || operator_status=$?
fi
[ "$operator_status" = 0 ] || [ "$operator_status" = 77 ] || exit "$operator_status"
if [ "$operator_status" = 77 ]; then
  echo "operator-tools-smoke: partial (shard $OPERATOR_SPEC; socket checks skipped)" >&2
elif [ "$OPERATOR_SPEC" = 1/1 ]; then
  echo 'operator-tools-smoke: ok'
else
  echo "operator-tools-smoke: ok (shard $OPERATOR_SPEC, $((SECONDS - OPERATOR_STARTED))s)"
fi
if [ "$operator_status" = 77 ]; then
  if [ -n "${OMS_OPERATOR_PARTIAL_MARKER_DIR:-}" ]; then
    : > "$OMS_OPERATOR_PARTIAL_MARKER_DIR/operator-partial.${OPERATOR_SPEC%/*}"
  fi
  exit 77
fi

# Kept inline for the dispatch-exit worker; runner.py materializes only this body.
: <<'PY'
# OPERATOR_SECTION_BEGIN dispatch_exit
def exercise_dispatch_exit_evidence(root, temporary, project, shared, room_owner, room_env):
    import errno
    import io
    import panel_dispatch
    import signal
    import stat
    from contextlib import redirect_stderr
    from unittest.mock import Mock, patch
    from panel_routing import allocate
    atomic_ready = ("def publish_ready(path, content):\n"
                    "    path = Path(path)\n"
                    "    sibling = path.with_name(path.name + '.' + str(os.getpid()) + '.tmp')\n"
                    "    sibling.write_text(content)\n"
                    "    os.replace(str(sibling), str(path))\n")
    readiness_probe = temporary / "atomic-readiness-probe"
    namespace = {"Path": Path, "os": os}
    exec(atomic_ready, namespace)
    def observe_readiness_write(path, content):
        with path.open("w") as stream:
            assert not readiness_probe.exists(), "readiness visible before write completed"
            stream.write(content)
    with patch.object(Path, "write_text", observe_readiness_write):
        namespace["publish_ready"](readiness_probe, "123")
    assert readiness_probe.read_text() == "123"
    readiness_probe.unlink()
    def complete_descendants(pid):
        observations = []
        for unused in range(3):
            observations.append(panel._dispatch_descendants(pid))
            if observations[-1] is not None:
                return observations[-1]
        raise AssertionError("incomplete descendant snapshots for pid %s: %r" % (pid, observations))

    with patch.object(panel, "_dispatch_descendants", side_effect=[None, [{"pid": 1}]]):
        assert complete_descendants(1) == [{"pid": 1}]
    with patch.object(panel, "_dispatch_descendants", return_value=[]):
        assert complete_descendants(1) == []
    with patch.object(panel, "_dispatch_descendants", side_effect=[None, None, []]) as final_attempt:
        assert complete_descendants(1) == [] and final_attempt.call_count == 3
    with patch.object(panel, "_dispatch_descendants", return_value=None) as incomplete:
        try:
            complete_descendants(1)
        except AssertionError as error:
            assert "incomplete" in str(error) and incomplete.call_count == 3, str(error)
        else:
            raise AssertionError("incomplete snapshots accepted")
    # Every interpreter, including the directly spawned supervisor, uses this
    # fixture's durable state home rather than the invoking user's state.
    os.environ["XDG_STATE_HOME"] = room_env["XDG_STATE_HOME"]
    Path(os.environ["XDG_STATE_HOME"]).mkdir(mode=0o700, parents=True, exist_ok=True)
    Path(os.environ["XDG_STATE_HOME"]).chmod(0o700)
    if os.name == "posix":
        durable_name = "call-" + panel.uuid.uuid4().hex[:16]
        previous_umask = os.umask(0)
        try:
            durable_call = panel._prepare_dispatch_call(project, shared, durable_name, room_owner,
                                                         execution_repo=project, task_id="durable-records")
        finally:
            os.umask(previous_umask)
        durable_root = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                        hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" / durable_name)
        launch_file = durable_root / (durable_name + "-launch.json")
        assert launch_file.is_file(), "dispatch launch was not stored under the durable per-call root"
        envelope = json.loads(launch_file.read_text())
        assert envelope["schema"] == 1 and envelope["record"]["call"] == durable_call, envelope
        identity = json.loads((durable_root / "identity.json").read_text())
        assert identity["schema"] == 1 and identity["repo"] == str(project.resolve()), identity
        assert identity["room"] == shared and identity["participant"] == durable_name, identity
        assert identity["parent"] == room_owner and identity["task_id"] == "durable-records", identity
        assert identity["nonce"] == durable_call["nonce"] and identity["controller"]["pid"] == os.getpid(), identity
        assert identity["controller"]["identity"], identity
        assert identity["boot_id"] == (Path("/proc/sys/kernel/random/boot_id").read_text().strip()
                                       if sys.platform == "linux" else "unknown"), identity
        assert all(stat.S_IMODE(path.stat().st_mode) == 0o600 for path in (launch_file, durable_root / "identity.json"))
        assert stat.S_IMODE(durable_root.stat().st_mode) == 0o700
        evidence_member = dict(participant=durable_name, parent=room_owner, joined=True, owns=["value.txt"])
        with patch.object(panel, "_dispatch_call_record", return_value=durable_call), \
                patch.object(panel, "_dispatch_released", return_value=False), \
                patch.object(panel, "_settlement_delivery_pending", return_value=False), \
                patch.object(panel, "_dispatch_pending", return_value=dict(
                    durable_call, exit=1, task_id="durable-records")), \
                patch.object(panel, "_dispatch_supervisor_receipt", side_effect=panel_dispatch.DispatchEvidenceError(
                    durable_name, "supervisor receipt", "malformed record")), \
                patch.object(panel, "events", return_value="[]"), \
                patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as notice:
            panel._reconcile_dispatch(project, shared, {"participants": [evidence_member]}, room_owner, room_owner)
            report = notice.getvalue()
        assert "dispatch reconciliation blocked: invalid supervisor receipt" in report, report
        assert "Recovery: inspect records with: ls -la --" in report, report
        probe = ("import json,sys; sys.path.insert(0,sys.argv[1]); import dispatch_supervisor as d; "
                 "assert d.read(sys.argv[2],sys.argv[3])['call']==json.loads(sys.argv[4])")
        fresh = subprocess.run([sys.executable, "-c", probe, str(root / "scripts/lib"), str(project),
                                durable_name + "-launch.json", json.dumps(durable_call)], capture_output=True, text=True)
        assert fresh.returncode == 0, fresh.stderr
        failures = []
        # A worker can fabricate internally consistent evidence in its own state
        # home. A fresh controller must discover records from its own environment.
        forged_name = "call-" + panel.uuid.uuid4().hex[:16]
        with patch.dict(os.environ, XDG_STATE_HOME=str(temporary / "worker-state")):
            forged_call = panel._prepare_dispatch_call(project, shared, forged_name, room_owner)
            forged_root = panel.dispatch_store.store(project) / (forged_name + "-launch.json")
            forged_call["control_root"] = str(forged_root.parent)
            diagnostic = project / panel._dispatch_call_path(forged_name)
            diagnostic.write_bytes(panel.dispatch_store.encoded(forged_call))
            anchor = panel.dispatch_store.read(project, forged_name + "-launch.json")
            anchor.update(call=forged_call, generation=panel.dispatch_store.generation(diagnostic))
            panel.dispatch_store.write(project, forged_name + "-launch.json", anchor)
            panel._record_dispatch_pending(project, forged_call, 0, "forged-root", {"descendants": []})
            panel.dispatch_store.write(project, forged_name + "-process.json", {"pid": os.getpid()})
            panel.dispatch_store.write(project, forged_name + "-supervisor.json",
                dict(pid=os.getpid(), all_reaped=True, exit=0,
                     launch_sha256=panel._dispatch_exit_record(forged_call, 0)["launch_sha256"]))
        probe = ("import json,sys; from pathlib import Path; sys.path.insert(0,sys.argv[1]); "
                 "import terminal_panel as p; "
                 "print(json.dumps(p._dispatch_call_finished(Path(sys.argv[2]),sys.argv[3],json.loads(sys.argv[4]))))")
        args = [sys.executable, "-c", probe, str(root / "scripts/lib"), str(project), shared,
                json.dumps({"participant": forged_name, "parent": room_owner})]
        for redirected in (True, False):
            if not redirected:
                diagnostic.unlink()
            fresh = subprocess.run(args, capture_output=True, text=True)
            assert fresh.returncode == 0, fresh.stderr
            if json.loads(fresh.stdout):
                failures.append("fresh reader settled forged records via repository redirect" if redirected else
                                "fresh reader settled records outside its trusted root")
        legacy_name = "call-" + panel.uuid.uuid4().hex[:16]
        legacy_root = temporary / "legacy-state" / "dispatch"
        with patch.object(panel.dispatch_store, "_legacy", return_value=legacy_root):
            with panel.dispatch_store.Directory(legacy_root, create=True) as directory:
                panel.dispatch_store._write(directory, legacy_name + "-process.json", {"pid": os.getpid()})
            if panel._review_launch_state(project, legacy_name) != "launched":
                failures.append("legacy process without launch was hidden instead of launched")
            assert not panel._dispatch_call_finished(project, shared,
                {"participant": legacy_name, "parent": room_owner}), "legacy process alone settled a call"
        identity_file = durable_root / "identity.json"
        original_identity, original_launch = identity_file.read_bytes(), launch_file.read_bytes()
        try:
            for missing_identity in (False, True):
                null_launch = dict(envelope, launch=None)
                launch_file.write_bytes(panel.dispatch_store.encoded(null_launch))
                if missing_identity:
                    identity_file.unlink()
                try:
                    panel.dispatch_store.read(project, durable_name + "-launch.json")
                except (OSError, ValueError):
                    pass
                else:
                    failures.append("durable read accepted missing identity and null launch")
            try:
                panel.dispatch_store.write(project, durable_name + "-process.json", {"pid": os.getpid()})
            except (OSError, ValueError):
                pass
            else:
                failures.append("durable write accepted missing identity")
        finally:
            identity_file.write_bytes(original_identity)
            identity_file.chmod(0o600)
            launch_file.write_bytes(original_launch)
        assert not failures, "dispatch provenance regressions: " + "; ".join(failures)
        unrelated = temporary / "unrelated-repo"
        subprocess.run(["git", "init", "-q", str(unrelated)], check=True)
        for unsafe_home in (project / "private-state", temporary / "execution-repo/private-state", unrelated / "state"):
            execution = temporary / "execution-repo"
            execution.mkdir(exist_ok=True)
            with patch.dict(os.environ, XDG_STATE_HOME=str(unsafe_home)):
                try:
                    panel._prepare_dispatch_call(project, shared, "call-" + panel.uuid.uuid4().hex[:16], room_owner,
                                                 execution_repo=execution)
                except ValueError:
                    pass
                else:
                    raise AssertionError("dispatch accepted a state root inside a repository")
        shared_parent = temporary / "other-writable-parent"
        shared_parent.mkdir(mode=0o775)
        shared_parent.chmod(0o775)
        try:
            original_stat = Path.stat
            def other_owned_stat(path, *args, **kwargs):
                info = original_stat(path, *args, **kwargs)
                if path == shared_parent:
                    fields = list(info)
                    fields[4] = os.getuid() + 1
                    return os.stat_result(fields)
                return info
            with patch.object(Path, "stat", autospec=True, side_effect=other_owned_stat):
                try:
                    panel.dispatch_store._validate_state_ancestors(shared_parent / "state")
                except ValueError as error:
                    assert "unsafe writable dispatch state ancestor" in str(error), error
                    assert "other-writable-parent" in str(error), error
                    assert "XDG_STATE_HOME" in str(error) and "chmod" in str(error), error
                else:
                    raise AssertionError("dispatch accepted an other-writable non-sticky state ancestor")
            try:
                panel.dispatch_store._validate_state_ancestors(shared_parent / "state")
            except ValueError as error:
                assert "unsafe writable dispatch state ancestor" in str(error), error
            else:
                raise AssertionError("dispatch accepted a self-owned writable ancestor")
            descriptor_child = shared_parent / "private-child"
            descriptor_child.mkdir(mode=0o700)
            descriptor_child.chmod(0o700)
            try:
                panel.dispatch_store.Directory(descriptor_child)
            except ValueError as error:
                assert "unsafe writable dispatch state ancestor" in str(error), error
            else:
                raise AssertionError("descriptor traversal accepted a self-owned writable ancestor")
        finally:
            shared_parent.chmod(0o700)
        restricted_path = temporary / "restrictive-umask" / "state"
        previous_umask = os.umask(0o400)
        try:
            try:
                with panel.dispatch_store.Directory(restricted_path, create=True):
                    pass
            except ValueError as error:
                assert "restrictive umask" in str(error), error
                assert "owner rwx" in str(error), error
                assert str(restricted_path.parent) in str(error), error
            else:
                raise AssertionError("restricted owner-read umask was not reported clearly")
        finally:
            os.umask(previous_umask)
        with panel.dispatch_store.Directory(restricted_path, create=True) as directory:
            assert stat.S_IMODE(os.fstat(directory.fd).st_mode) == 0o700
        original = launch_file.read_bytes()
        launch_file.unlink()
        target = temporary / "linked-dispatch-record"
        target.write_bytes(original)
        target.chmod(0o600)
        launch_file.symlink_to(target)
        try:
            panel.dispatch_store.read(project, durable_name + "-launch.json")
        except (OSError, ValueError):
            pass
        else:
            raise AssertionError("dispatch followed a symlinked record")
        launch_file.unlink()
        launch_file.write_bytes(original)
        launch_file.chmod(0o600)
        actual_fstat = os.fstat
        def foreign_record(fd):
            info = actual_fstat(fd)
            return type("ForeignOwner", (), dict(st_mode=info.st_mode, st_nlink=info.st_nlink,
                                                  st_uid=os.getuid() + 1))() if stat.S_ISREG(info.st_mode) else info
        with patch.object(panel.dispatch_store.os, "fstat", side_effect=foreign_record):
            try:
                panel.dispatch_store.read(project, durable_name + "-launch.json")
            except ValueError:
                pass
            else:
                raise AssertionError("dispatch accepted a foreign-owned record")
        if sys.platform == "linux":
            with patch.object(panel.subprocess, "Popen", side_effect=AssertionError("metadata spawned a PID probe")):
                panel.dispatch_store.write(project, durable_name + "-process.json", {"pid": 2147483647})
    main = {"attempt_id": "round-main", "terminal": False}
    history = [dict(attempt_id="round-%d" % i, task_id="round-task", parent_attempt_id="round-main",
                    sequence=i, state="review", refs={"panel_role": "worker", "panel_access": "write"})
               for i in range(3)]
    with patch.object(panel, "owning_attempt", return_value={"round-main"}), \
            patch.object(panel, "events", side_effect=lambda *a, **k: json.dumps([main] + history)), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"attempt_id": "round-2", "exit": 0}]}]}):
        round_brief = temporary / "round-brief.md"
        round_brief.write_text("Fix the remaining reviewed finding.\n")
        continued, native, note = panel.continuation(project, "claude", None, "round-task", {"provider": "codex"},
            round_brief, {})
        assert native is None and "no patch" in note and "no resumable session" in note
        continued.unlink()
        with patch.object(panel, "results", return_value={"rows": [{"calls": [{"attempt_id": "round-2", "exit": 0,
                                                                                 "patch": ".oms/artifacts/large.patch"}]}]}), \
                patch.object(panel, "verified_patch", return_value=b"x" * panel.MAX_PROMPT):
            try:
                panel.continuation(project, "claude", None, "round-task", {"provider": "codex"}, round_brief, {})
            except ValueError as error:
                assert "exceeds the continuation input budget" in str(error), error
            else:
                raise AssertionError("continuation promised an inaccessible oversized patch")
        with patch.object(panel, "results", return_value={"rows": [{"calls": [{"attempt_id": "round-2", "exit": 124}]}]}), \
                patch.dict(history[-1]["refs"], panel_location="worktree:oh-my-setting-delegate.stopped/wt-stopped"):
            try:
                panel.continuation(project, "claude", None, "round-task", {"provider": "codex"}, round_brief, {})
            except ValueError as error:
                assert "--salvage round-task --attempt round-2" in str(error), error
            else:
                raise AssertionError("stopped write work bypassed salvage")
        # A review/admit/land tail past the 8-call display slice must not hide the worker's own call,
        # and a present-but-false guard key is not a guard stop.
        tail = [{"attempt_id": "later-%d" % i, "exit": 0} for i in range(9)]
        hidden = [dict(kind="delegate", attempt_id="round-2", task_id="round-task", exit=0, verify_exit=0,
                       guard=False, event_id="e-round-2")]
        with patch.object(panel, "results", return_value={"rows": [{"calls": tail}]}), \
                patch.object(panel_results, "_records", return_value=([main] + history, hidden)), \
                patch.dict(history[-1]["refs"], panel_location="worktree:oh-my-setting-delegate.stopped/wt-stopped"):
            continued, _, note = panel.continuation(project, "claude", None, "round-task", {"provider": "codex"},
                                                    round_brief, {})
            continued.unlink()
            assert "no patch" in note, note
        # A patch HEAD already contains is not replayed at the old base; any other patch still is.
        head_patch = b"--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-older\n+old\n"
        for body, contained in ((head_patch, True), (head_patch.replace(b"-older\n+old", b"-old\n+older"), False)):
            record = dict(kind="delegate", attempt_id="round-2", patch=".oms/artifacts/p.patch", patch_sha256="a" * 64,
                          event_id="e-round-2", base_sha="a" * 12)
            call = dict(attempt_id="round-2", exit=0, patch=".oms/artifacts/p.patch", patch_sha256="a" * 64,
                        event_id="e-round-2")
            reference = {}
            with patch.object(panel, "results", return_value={"rows": [{"calls": [call]}]}), \
                    patch.object(panel, "verified_patch", return_value=body), \
                    patch.object(panel_results, "_records", return_value=([main] + history, [record])):
                continued, _, note = panel.continuation(project, "claude", None, "round-task", {"provider": "codex"},
                    round_brief, {}, patch_reference=reference)
            text = continued.read_text()
            continued.unlink()
            if contained:
                assert reference == {} and "already part of the repository at HEAD" in text, (reference, text)
            else:
                assert reference["base"] == "a" * 12
                assert "original base, not the current HEAD" in text, text
    # A continuation starts with the complete prior patch and returns one patch
    # against the original base, including when the owner has since moved HEAD.
    round_bin = temporary / "continuation-bin"
    round_bin.mkdir()
    original_cli = Path(room_env["PATH"].split(os.pathsep)[0]) / "codex"
    round_cli = round_bin / "codex"
    probe = '''  if [ -n "${CONTINUATION_EXPECT:-}" ]; then
    python3 - <<'CONTINUATION'
import json, os, subprocess
from pathlib import Path
value = Path("value.txt").read_bytes()
assert value == bytes.fromhex(os.environ["CONTINUATION_EXPECT"]), value
Path(os.environ["CONTINUATION_SNAPSHOT"]).write_text(json.dumps({
    "head": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip(),
    "value": value.hex(),
    "staged": subprocess.check_output(["git", "diff", "--cached", "--name-only"], text=True).strip()}))
with Path("value.txt").open("ab") as handle:
    handle.write(b"caf\\xe9\\n")
CONTINUATION
  fi
'''
    marker = '  if [ "${PANEL_TEST_MODE:-}" = write ]; then\n'
    assert marker in original_cli.read_text()
    round_cli.write_text(original_cli.read_text().replace(marker, probe + marker))
    round_cli.chmod(0o755)
    round_env = dict(room_env, PATH=str(round_bin) + os.pathsep + room_env["PATH"], PANEL_TEST_MODE="write",
                     CONTINUATION_SNAPSHOT=str(temporary / "continuation-start.json"))
    worker = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
              "--dispatch", "builder", "--owner", "claude", "--to", "codex", "--model", "gpt-6-luna",
              "--access", "write", "--verify", "true", "--scope", "value.txt"]
    def round_call(argv, env=None):
        run = subprocess.run(argv, cwd=str(project), env=env or room_env, capture_output=True, text=True,
                             timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
        assert run.returncode == 0, (argv, run.stdout, run.stderr)
        return run.stdout
    original_base = round_call(["git", "rev-parse", "HEAD"]).strip()
    round_brief = temporary / "cumulative-brief.md"
    round_brief.write_text("Append new to value.txt.\n")
    first = round_call(worker + ["--brief-file", str(round_brief), "--task-id", "cumulative-rounds"],
                 dict(round_env, CONTINUATION_EXPECT=b"old\n".hex()))
    assert json.loads(Path(round_env["CONTINUATION_SNAPSHOT"]).read_text())["staged"] == ""
    first_patch = Path(re.search(r"^patch: (.+)$", first, re.M)[1])
    assert b"+caf\xe9\n" in first_patch.read_bytes(), "fixture must capture a non-UTF-8 text hunk"
    round_brief.write_text("Append new once more to value.txt.\n")
    tree = round_call(["git", "rev-parse", "HEAD^{tree}"]).strip()
    moved_head = round_call(["git", "commit-tree", tree, "-p", original_base, "-m", "Owner advances HEAD"]).strip()
    round_call(["git", "update-ref", "HEAD", moved_head])
    try:
        second = round_call(worker + ["--brief-file", str(round_brief), "--continue", "cumulative-rounds"],
                      dict(round_env, CONTINUATION_EXPECT=b"old\ncaf\xe9\nnew\n".hex()))
    finally:
        round_call(["git", "update-ref", "HEAD", original_base])
    start = json.loads(Path(round_env["CONTINUATION_SNAPSHOT"]).read_text())
    assert start == {"head": original_base, "value": b"old\ncaf\xe9\nnew\n".hex(), "staged": "value.txt"}, start
    artifact_patch = Path(re.search(r"^patch: (.+)$", second, re.M)[1])
    assert artifact_patch != first_patch
    product = [json.loads(line) for line in (project / ".oms/artifacts/index.jsonl").read_text().splitlines()][-1]
    assert original_base.startswith(product["base_sha"]), product
    round_call(["git", "apply", "--check", str(artifact_patch)])
    try:
        round_call(["git", "apply", str(artifact_patch)])
        assert (project / "value.txt").read_bytes() == b"old\ncaf\xe9\nnew\ncaf\xe9\nnew\n"
    finally:
        round_call(["git", "checkout", "--", "value.txt"])
    # Digest drift and a verified but non-applicable patch both stop before a
    # provider starts; application failure must never fall back to a clean tree.
    bad_patch = project / ".oms/artifacts/continuation-conflict.patch"
    bad_patch.write_bytes(artifact_patch.read_bytes().replace(b" old\n", b" absent\n"))
    assert bad_patch.read_bytes() != artifact_patch.read_bytes()
    digest = hashlib.sha256(bad_patch.read_bytes()).hexdigest()
    sensitive_patch = bad_patch.with_name("continuation-sensitive.patch")
    sensitive_patch.write_bytes(artifact_patch.read_bytes().replace(b"+new\n", b"+-----BE" + b"GIN PRIVATE KEY-----\n"))
    # Repository context in a continuation patch is not outbound content; only added lines are scanned.
    scratch_line = "/scratc" + "h/y.txt"
    head_lines = "--- a/value.txt\n+++ b/value.txt\n@@ -1 +1,2 @@\n old\n"
    context_patch = bad_patch.with_name("continuation-context.patch")
    context_patch.write_text(head_lines.replace("+1,2", "+1,3") + " " + scratch_line + "\n+new\n")
    added_patch = bad_patch.with_name("continuation-added.patch")
    added_patch.write_text(head_lines + "+" + scratch_line + "\n")
    before_workers = Path(room_env["PANEL_TEST_WORKER_LOG"]).read_bytes()
    for previous, sha, message in ((bad_patch, "0" * 64, "could not be verified"),
            (bad_patch, digest, "no longer applies"),
            (sensitive_patch, hashlib.sha256(sensitive_patch.read_bytes()).hexdigest(), "failed outbound validation"),
            (context_patch, hashlib.sha256(context_patch.read_bytes()).hexdigest(), "no longer applies"),
            (added_patch, hashlib.sha256(added_patch.read_bytes()).hexdigest(), "continuation patch failed outbound")):
        reference = dict(patch=previous.relative_to(project).as_posix(), sha256=sha, base=original_base)
        refused = subprocess.run(["bash", str(panel.ENTRY), "peer-delegate", "--repo", str(project),
            "--to", "codex", "--model", "gpt-6-luna", "--brief-file", str(round_brief), "--verify", "true"],
            cwd=str(project), env=dict(round_env, OMS_PANEL_DISPATCH="1", OMS_PANEL_CONTINUATION=json.dumps(reference)),
            capture_output=True, text=True, timeout=90)
        assert refused.returncode != 0 and message in refused.stderr, refused
        assert Path(room_env["PANEL_TEST_WORKER_LOG"]).read_bytes() == before_workers
    assert (project / "value.txt").read_text() == "old\n"
    if os.name != "posix":
        return
    import panel_terminals
    # PID reuse: the identity lookup reports an impostor. pidfd pins the original
    # process, so the impostor is rejected before the pinned handle signals; the
    # fallback re-checks identity right before os.kill. Neither path signals it.
    reused_pid = 2147483645
    def reuse_probe(pidfd, identity_now):
        read_end, write_end = os.pipe()
        os.close(write_end)
        opened = Mock(return_value=read_end) if pidfd else None
        sender = Mock() if pidfd else None
        found = {"pid": reused_pid, "identity": identity_now, "state": "S"}
        with patch.object(panel_terminals.os, "pidfd_open", opened, create=True), \
                patch.object(panel_terminals.signal, "pidfd_send_signal", sender, create=True), \
                patch.object(panel_terminals, "process", return_value=found) as lookup, \
                patch.object(panel_terminals.os, "kill") as numeric:
            sent = panel_terminals.signal_process(reused_pid, "original", signal.SIGTERM)
            lookup.assert_called_once_with(reused_pid)
        if not pidfd:
            os.close(read_end)
        return sent, opened, sender, numeric, read_end
    for pidfd in (True, False):
        sent, opened, sender, numeric, handle = reuse_probe(pidfd, "reused")
        assert sent is False
        numeric.assert_not_called()
        if pidfd:
            opened.assert_called_once_with(reused_pid, 0)
            sender.assert_not_called()
            try:
                os.fstat(handle)
            except OSError:
                pass
            else:
                raise AssertionError("pidfd leaked after an impostor was rejected")
        sent, opened, sender, numeric, handle = reuse_probe(pidfd, "original")
        assert sent is True
        if pidfd:
            sender.assert_called_once_with(handle, signal.SIGTERM)
            numeric.assert_not_called()
            try:
                os.fstat(handle)
            except OSError:
                pass
            else:
                raise AssertionError("pidfd leaked after signalling")
        else:
            numeric.assert_called_once_with(reused_pid, signal.SIGTERM)
    # A pidfd the host refuses (seccomp, old kernel) degrades to the fallback.
    with patch.object(panel_terminals.os, "pidfd_open", side_effect=OSError(errno.EPERM, "denied"), create=True), \
            patch.object(panel_terminals.signal, "pidfd_send_signal", Mock(), create=True) as sender, \
            patch.object(panel_terminals, "process", return_value={"pid": reused_pid, "identity": "original", "state": "S"}), \
            patch.object(panel_terminals.os, "kill") as numeric:
        assert panel_terminals.signal_process(reused_pid, "original", signal.SIGTERM)
        numeric.assert_called_once_with(reused_pid, signal.SIGTERM)
        sender.assert_not_called()
    original_members = {p["participant"] for p in room.status(project, shared)["participants"]}
    # PID zero is a process-group selector, not a child identity (argv-observer
    # uses that sentinel). Never let an invalid or self PID reach signal APIs.
    for pid in (0, -1, os.getpid()):
        invalid = Mock(pid=pid, poll=lambda: 0)
        with patch.object(panel.os, "killpg") as groups, patch.object(panel.os, "kill") as pids:
            assert panel._stop_dispatch(invalid, None) is False
            assert panel._stop_dispatch_groups(invalid, None, {}) is False
        groups.assert_not_called()
        pids.assert_not_called()
        invalid.terminate.assert_not_called()
    # Controller provenance survives a fresh CLI; repository substitutions never do.
    route = allocate("claude", "worker", "light", "read", "investigate")
    def dispatch_case(code, task, platform=None):
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", code, "--repo", str(project)]), \
                patch.object(panel, "plan_link", return_value=None), \
                redirect_stderr(io.StringIO()) as notices:
            before = {p["participant"] for p in room.status(project, shared)["participants"]}
            if platform:
                with patch.object(panel.sys, "platform", platform):
                    status = panel.run_dispatch(project, "claude", "worker", "read", "Inspect", None, None,
                                                task, False, None, route, None, None, ["value.txt"])
            else:
                status = panel.run_dispatch(project, "claude", "worker", "read", "Inspect", None, None,
                                            task, False, None, route, None, None, None)
            state = room.status(project, shared)
            member = next(p for p in state["participants"] if p["participant"] not in before)
            return status, member, notices.getvalue()

    if sys.platform == "linux":
        import hook_state
        ready_tasks = [{"id": "waiting", "state": "ready", "depends": ["missing"], "created": "0"},
                       {"id": "finished", "state": "done"},
                       {"id": "next-task", "state": "ready", "depends": ["finished"], "created": "1"},
                       {"id": "later-task", "state": "ready", "created": "2"}]
        with patch.object(hook_state, "plan_tasks", return_value=({}, ready_tasks)) as read_plan:
            ready_status, ready_member, ready_notice = dispatch_case("pass", "next-ready-hint")
        read_plan.assert_called_once_with(project)
        assert ready_status == 0, ready_notice
        assert "next ready: next-task" in ready_notice and "waiting" not in ready_notice and "later-task" not in ready_notice, ready_notice
        assert "oms agent-plan claim --id next-task --provider claude" in ready_notice, ready_notice
        assert "oms panel --dispatch ROLE --owner claude --task-id next-task" in ready_notice, ready_notice
        assert not hook_state.panel_next_ready_hint([], "claude")
        assert not hook_state.panel_next_ready_hint(ready_tasks[:2], "claude")

    if sys.platform == "linux":
        from contextlib import contextmanager
        import ctypes
        libc, previous = ctypes.CDLL(None, use_errno=True), ctypes.c_int()
        assert libc.prctl(37, ctypes.byref(previous), 0, 0, 0) == 0, "parent subreaper state unreadable"

        @contextmanager
        def recorded_fixture(task, body=None, member_id=None):
            name = member_id or "call-" + panel.uuid.uuid4().hex[:16]
            room.join(project, shared, name, "codex", "worker", parent=room_owner)
            room.scope(project, shared, name, ["value.txt"])
            member = room.participant(room.status(project, shared), name)
            call = panel._prepare_dispatch_call(project, shared, name, room_owner)
            digest = panel._dispatch_exit_record(call, 0)["launch_sha256"]
            release = temporary / (name + ".release")
            ready = temporary / (name + ".ready")
            attempt_path = temporary / (name + ".attempt")
            header = ("import os,sys,time,subprocess; from pathlib import Path\n"
                      "sys.path.insert(0," + repr(str(root / "scripts/lib")) + ")\n"
                      "import terminal_panel as panel, room\n" + atomic_ready +
                      "repo=Path(os.environ['OMS_ROOM_REPO']); ident=os.environ['OMS_ROOM_ID']; "
                      "member=os.environ['OMS_ROOM_PARTICIPANT']\n"
                      "attempt=panel.events(repo,'start','--provider','codex','--tool','peer-delegate',"
                      "'--task-id'," + repr(task) + ",'--parent-attempt-id',os.environ['OMS_PANEL_MAIN_ATTEMPT'],"
                      "'--ref','panel_role=worker','--ref','panel_room_id='+ident,"
                      "'--ref','panel_room_participant='+member,'--then','starting','--then','working',output=True)\n"
                      "Path(" + repr(str(attempt_path)) + ").write_text(attempt)\n"
                      "release=" + repr(str(release)) + "; ready=" + repr(str(ready)) + "\n")
            if body is None and task == "cancel-selected-call":
                body = ("child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])\n"
                        "publish_ready(" + repr(str(ready)) + ", str(child.pid)); time.sleep(60)\n")
            body = body or ("publish_ready(ready, str(os.getpid()))\n"
                            "while not Path(release).exists(): time.sleep(.02)\n")
            env = dict(room_env, OMS_ROOM_PARTICIPANT=name, OMS_ROOM_ADMITTED_PARTICIPANT=name)
            process = subprocess.Popen([sys.executable, str(root / "scripts/lib/dispatch_supervisor.py"),
                                        str(project), name, digest, sys.executable, "-c", header + body],
                                       env=env, start_new_session=True)
            try:
                supervisor = panel_terminals.process(process.pid)
                assert supervisor and supervisor["group"] == process.pid, "supervisor process identity unavailable"
                panel.dispatch_store.write(project, name + "-process.json",
                                           {"pid": process.pid, "identity": supervisor["identity"],
                                            "group": supervisor["group"]}, exclusive=True)
                deadline = time.monotonic() + 10
                while not ready.exists() and process.poll() is None and time.monotonic() < deadline:
                    time.sleep(.02)
                assert ready.exists(), "normal child did not become ready"
                assert process.poll() is None, "fixture supervisor exited before its descendants"
                panel._record_dispatch_pending(project, call, 1, task, {"descendants": complete_descendants(process.pid)})
                row = json.loads(panel.events(project, "show", "--attempt", attempt_path.read_text(), "--json", output=True))
                yield dict(name=name, member=member, call=call, process=process, release=release,
                           ready=ready, row=row, digest=digest, supervisor=supervisor)
            finally:
                release.touch()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    process.wait(timeout=25)
                state = room.status(project, shared)
                if room.participant(state, name)["joined"]:
                    room.append(project, shared, {"kind": "leave", "participant": name}, "Recorded fixture complete")
                current = json.loads(panel.events(project, "show", "--attempt", row["attempt_id"], "--json", output=True)) if 'row' in locals() else None
                if current and not current["terminal"]:
                    panel.events(project, "transition", "--attempt", row["attempt_id"], "--state", "cancelled",
                                 "--reason-code", "fixture_complete")

        def mark_stale_boot_records(name, suffixes=("launch", "process", "pending", "supervisor")):
            for suffix in suffixes:
                record_name = name + "-" + suffix + ".json"
                record_path = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                               hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" /
                               name / record_name)
                envelope = json.loads(record_path.read_text())
                envelope["boot_id"] = "00000000-0000-0000-0000-000000000000"
                record_path.write_bytes(panel.dispatch_store.encoded(envelope))

        with recorded_fixture("malformed-same-boot-dispatch") as fixture:
            name, member, call = (fixture[key] for key in ("name", "member", "call"))
            state_member = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                            hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" / name)
            panel.dispatch_store.write(project, name + "-supervisor.json", {})
            with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as notice:
                panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                report = notice.getvalue()
            assert "dispatch reconciliation blocked: invalid supervisor receipt" in report, report
            assert "Recovery: inspect records with: ls -la --" in report, report
            current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                              "--json", output=True))
            assert current["state"] == "working" and not current["terminal"], current
            (state_member / (name + "-process.json")).write_bytes(b"{")
            with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as notice:
                panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                report = notice.getvalue()
            assert "dispatch reconciliation blocked: invalid process binding" in report, report
            assert "Recovery: inspect records with: ls -la --" in report, report

        with recorded_fixture("stale-boot-dispatch") as fixture:
            name, member, call = (fixture[key] for key in ("name", "member", "call"))
            if sys.platform == "linux":
                state_member = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                                hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" / name)
                for suffix in ("pending", "supervisor", "process"):
                    (state_member / (name + "-" + suffix + ".json")).unlink()
                mark_stale_boot_records(name, ("launch",))
                assert panel.dispatch_store.stale_boot(project, name + "-launch.json")
                panel._record_dispatch_pending(project, call, 0, task_id="stale-evidence-test",
                                               evidence={"descendants": []})
                mark_stale_boot_records(name, ("pending",))

                def assert_bad_stale_evidence(label, reason):
                    try:
                        finished = panel._dispatch_call_finished(project, shared, member)
                    except panel_dispatch.DispatchEvidenceError:
                        finished = False
                    assert not finished, label + " exit evidence incorrectly settled the call"
                    with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as notice:
                        panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                        report = notice.getvalue()
                    assert label in report, report
                    assert reason in report, report
                    assert "Recovery:" in report and "ls -la --" in report, report
                    assert str(state_member) in report and "retry the dispatch" in report, report
                    current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                                      "--json", output=True))
                    assert current["state"] == "working" and not current["terminal"], current

                supervisor_path = state_member / (name + "-supervisor.json")
                process_path = state_member / (name + "-process.json")
                panel.dispatch_store.write(project, name + "-supervisor.json", {})
                assert_bad_stale_evidence("supervisor receipt", "malformed record")
                valid_process = {"pid": os.getpid(), "identity": panel.dispatch_store.process_identity()["identity"],
                                 "group": os.getpid()}
                panel.dispatch_store.write(project, name + "-process.json", valid_process)
                receipt = {"launch_sha256": "0" * 64, "pid": os.getpid(), "all_reaped": True, "exit": 0}
                panel.dispatch_store.write(project, name + "-supervisor.json", receipt)
                assert_bad_stale_evidence("supervisor receipt", "bound to another launch")
                supervisor_path.unlink()
                envelope = json.loads(process_path.read_text())
                envelope["launch"] = dict(envelope["launch"], nonce="foreign-launch")
                process_path.write_bytes(panel.dispatch_store.encoded(envelope))
                assert_bad_stale_evidence("process binding", "malformed or foreign record")
                for path in (supervisor_path, process_path, state_member / (name + "-pending.json")):
                    if path.exists():
                        path.unlink()
                assert panel._dispatch_supervisor_snapshot(project, call) is None, \
                    "stale supervisor identity remained signalable"
                assert panel._review_launch_state(project, name) == "interrupted", \
                    "stale reviewer process counted as launched"
                assert panel._dispatch_pending(project, call)["exit"] == 143, \
                    "stale dispatch did not settle with interrupted status"
                assert panel._dispatch_call_finished(project, shared, member), \
                    "stale process record blocked interrupted settlement"
                panel._settle_dispatch(project, shared, name, 0)
                assert panel.dispatch_store.read(project, name + "-settled.json")["exit"] == 143, \
                    "missing exit evidence did not settle as interrupted"
                settled = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                                  "--json", output=True))
                assert settled["terminal"] and settled["state"] == "cancelled", settled
                panel.dispatch_store.write(project, name + "-process.json",
                                           {"pid": os.getpid(),
                                            "identity": panel.dispatch_store.process_identity()["identity"],
                                            "group": os.getpid()})
                panel.dispatch_store.write(project, name + "-supervisor.json",
                                           {"launch_sha256": panel._dispatch_exit_record(call, 0)["launch_sha256"],
                                            "pid": os.getpid() + 1, "all_reaped": True, "exit": 0})
                with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as released_notice:
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                assert "dispatch reconciliation blocked" not in released_notice.getvalue(), \
                    "released member still reported stale invalid evidence"
                for suffix in ("process", "supervisor"):
                    (state_member / (name + "-" + suffix + ".json")).unlink()

                orphan_name = "call-orphan-fixture"
                orphan_path = panel.dispatch_store._root(project) / orphan_name
                orphan_path.mkdir(mode=0o700)
                orphan_path.chmod(0o700)
                with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as orphan_notice:
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                    first_notice = orphan_notice.getvalue()
                assert "orphan dispatch member " + orphan_name + " has no identity" in first_notice, first_notice
                assert "remove it with: rm -r --" in first_notice, first_notice
                assert orphan_path.is_dir(), "orphan member was automatically removed"
                with patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as orphan_notice:
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                assert orphan_name not in orphan_notice.getvalue(), "orphan warning repeated"

        if sys.platform == "linux":
            with recorded_fixture("stale-boot-dispatch-success") as fixture:
                name, member, call, process = (fixture[key] for key in ("name", "member", "call", "process"))
                fixture["release"].touch()
                timeout = int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90"))
                assert process.wait(timeout=timeout) == 0, "successful stale-boot fixture did not exit"
                panel._record_dispatch_pending(project, call, 0, "stale-boot-dispatch-success",
                                               {"descendants": []})
                receipt = panel._dispatch_supervisor_receipt(project, call)
                assert receipt and receipt["all_reaped"] and receipt["exit"] == 0, receipt
                state_member = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                                hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" / name)
                receipt["all_reaped"], receipt["exit"] = False, 7
                panel.dispatch_store.write(project, name + "-supervisor.json", receipt)
                mark_stale_boot_records(name, ("launch", "pending"))
                mark_stale_boot_records(name, ("process", "supervisor"))
                pending = panel._dispatch_pending(project, call)
                assert pending["exit"] == 7, "stale boot discarded the persisted provider exit"
                assert panel._dispatch_supervisor_snapshot(project, call) is None, \
                    "stale supervisor identity remained signalable"
                assert panel._dispatch_call_finished(project, shared, member), \
                    "stale successful call was not treated as finished"
                with patch.dict(os.environ, room_env, clear=True):
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                settled = panel.dispatch_store.read(project, name + "-settled.json")
                assert settled["exit"] == 7, "stale settlement discarded the provider exit"
                current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                                  "--json", output=True))
                assert current["terminal"] and current["state"] == "failed", current

            with recorded_fixture("stale-boot-dispatch-pending-zero") as fixture:
                name, member, call, process = (fixture[key] for key in ("name", "member", "call", "process"))
                fixture["release"].touch()
                timeout = int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90"))
                assert process.wait(timeout=timeout) == 0, "successful pending-zero fixture did not exit"
                panel._record_dispatch_pending(project, call, 0, "stale-boot-pending-zero",
                                               {"descendants": []})
                state_member = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                                hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" / name)
                for suffix in ("process", "supervisor"):
                    (state_member / (name + "-" + suffix + ".json")).unlink()
                mark_stale_boot_records(name, ("launch", "pending"))
                pending = panel._dispatch_pending(project, call)
                assert pending["exit"] == 0, "stale boot discarded the pending successful exit"
                panel._settle_dispatch(project, shared, name, pending["exit"])
                assert panel.dispatch_store.read(project, name + "-settled.json")["exit"] == 0, \
                    "stale settlement discarded the pending successful exit"

            with recorded_fixture("stale-boot-dispatch-disappears") as fixture:
                name, member, call = (fixture[key] for key in ("name", "member", "call"))
                original_stale_boot = panel.dispatch_store.stale_boot
                disappeared = {"done": False}

                def disappear_supervisor_once(repo, record_name):
                    if record_name == name + "-supervisor.json" and not disappeared["done"]:
                        disappeared["done"] = True
                        raise FileNotFoundError(record_name)
                    return original_stale_boot(repo, record_name)

                with patch.object(panel.dispatch_store, "stale_boot", side_effect=disappear_supervisor_once), \
                        patch.dict(os.environ, room_env, clear=True), redirect_stderr(io.StringIO()) as notice:
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                    report = notice.getvalue()
                assert disappeared["done"], "supervisor disappearance race was not exercised"
                assert "dispatch reconciliation blocked" not in report, report
                current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                                  "--json", output=True))
                assert not current["terminal"], "missing supervisor evidence settled the call prematurely"

        # Public artifacts never replace positive controller proof, even with terminal ledger rows.
        with recorded_fixture("cancel-selected-call") as fixture:
            import room_view
            name, member, call, process = (fixture[key] for key in ("name", "member", "call", "process"))
            selected_state = {"room": room.status(project, shared),
                              "room_attempts": {name: {"attempt_id": fixture["row"]["attempt_id"],
                                                       "state": "working"}}}
            main_attempt = room_env["OMS_PANEL_MAIN_ATTEMPT"]
            tree_report, unused_status = panel.snapshot(project, room_id=shared)
            selected_nav = {"selected": ("result", name)}
            room_view.render_graph(tree_report, 120, 40, main_attempt=main_attempt, navigation=selected_nav)
            assert selected_nav.get("selected") == ("result", name), selected_nav
            assert selected_nav.get("primary") == ("chat", room_owner), selected_nav
            assert selected_nav.get("cancel_owner") == {"participant": room_owner, "attempt": main_attempt}, selected_nav
            from unittest.mock import Mock
            nonce_factory = Mock(return_value=Mock(hex="0123456789abcdef0123456789abcdef"))
            with patch.object(panel, "_cancel_window_id", return_value="@42"), \
                    patch.object(panel.uuid, "uuid4", nonce_factory), \
                    patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as confirm:
                pending = panel._begin_cancel_confirmation(
                    {"participant": name, "role": "worker", "model": "gpt-6-sol",
                     "label": "#(touch /tmp/pwn) #{window_name};"},
                    {"attempt_id": fixture["row"]["attempt_id"]}, "/dev/pts/7")
            confirm_command = confirm.call_args.args[0]
            assert confirm_command[:6] == ["tmux", "confirm-before", "-b", "-t", "/dev/pts/7", "-p"], confirm_command
            assert confirm_command[6] == \
                "Cancel worker gpt-6-sol ##(touch /tmp/pwn) ##{window_name};? (y/n)", confirm_command
            assert confirm_command[7] == "set-option -w -t @42 @oms_cancel_confirm " + pending["nonce"], confirm_command
            assert pending["window_id"] == "@42" and pending["call_id"] == name

            for clients in ("", "/dev/pts/7 @43\n/dev/pts/8 @44\n",
                            "/dev/pts/7 @42\n/dev/pts/8 @42\n"):
                with patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-fixture"}), \
                        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                            [], 0, clients, "")):
                    assert panel._cancel_client_name("@42") is None, clients
            with patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-fixture"}), \
                    patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, "/dev/pts/7 @42\n/dev/pts/8 @43\n", "")):
                assert panel._cancel_client_name("@42") == "/dev/pts/7"

            with patch.object(panel, "managed_session", return_value=True), \
                    patch.object(panel, "cancel_selected_call", return_value="ready"), \
                    patch.object(panel, "_cancel_window_id", return_value="@42"), \
                    patch.object(panel, "_cancel_client_name", return_value=None), \
                    patch.object(panel.subprocess, "run") as no_client_prompt:
                no_client_message, no_client_pending = panel._request_cancel_confirmation(
                    project, selected_state, selected_nav, main_attempt)
            assert no_client_message == "Cancel needs a single attached client" and no_client_pending is None
            no_client_prompt.assert_not_called()

            with patch.object(panel, "managed_session", return_value=False), \
                    patch.object(panel, "cancel_selected_call") as inline_cancel, \
                    patch.object(panel, "_begin_cancel_confirmation") as inline_prompt:
                inline_message, inline_pending = panel._request_cancel_confirmation(
                    project, selected_state, selected_nav, main_attempt)
            assert inline_message == "Cancel needs the managed tmux panel" and inline_pending is None
            inline_cancel.assert_not_called()
            inline_prompt.assert_not_called()

            tick_state = {"room_attempts": {name: {"attempt_id": pending["attempt_id"]}}}
            tick_nav = {"selected": ("result", name), "cancel_pending": pending}
            with patch.object(panel, "_cancel_option_value", return_value=pending["nonce"]), \
                    patch.object(panel, "_clear_cancel_option") as clear_option, \
                    patch.object(panel, "cancel_selected_call", return_value="Cancellation requested") as cancel_once:
                assert panel._poll_cancel_confirmation(project, tick_state, tick_nav, main_attempt,
                                                       now=pending["expires"] - 1) == "Cancellation requested"
            cancel_once.assert_called_once_with(project, tick_state, tick_nav, main_attempt, send=True)
            clear_option.assert_called_once_with("@42")

            for wrong_value, selected, now in (("wrong-nonce", ("result", name), pending["expires"] - 1),
                                               (pending["nonce"], ("result", "other-call"), pending["expires"] - 1),
                                               (pending["nonce"], ("result", name), pending["expires"] + 1)):
                stale = dict(pending)
                nav = {"selected": selected, "cancel_pending": stale}
                with patch.object(panel, "_cancel_option_value", return_value=wrong_value), \
                        patch.object(panel, "_clear_cancel_option"), \
                        patch.object(panel, "cancel_selected_call") as never_cancel:
                    assert panel._poll_cancel_confirmation(project, tick_state, nav, main_attempt, now=now) != "Cancellation requested"
                never_cancel.assert_not_called()
                assert "cancel_pending" not in nav

            from panel_input import TerminalInput
            pasted = TerminalInput().decode(b"\033[200~cy\033[201~")
            assert pasted == [("paste",)] and not any(event == ("y",) for event in pasted), pasted
            unbracketed_paste = TerminalInput().decode(b"cy")
            assert unbracketed_paste == [("c",), ("unbound",)], unbracketed_paste
            with patch.object(panel, "_cancel_option_value", return_value=None), \
                    patch.object(panel, "_clear_cancel_option") as no_paste_clear, \
                    patch.object(panel, "cancel_selected_call") as no_paste_cancel:
                waiting_nav = {"selected": ("result", name), "cancel_pending": dict(pending)}
                assert panel._poll_cancel_confirmation(project, tick_state, waiting_nav, main_attempt,
                                                       now=pending["expires"] - 1) is None
            no_paste_cancel.assert_not_called()
            no_paste_clear.assert_not_called()

            watch_options = {"@oms_panel_main_attempt": main_attempt,
                             "@oms_panel_room_participant": room_owner,
                             "@oms_panel_repo": str(project.resolve()),
                             "@oms_panel_main_watch": "1",
                             "@oms_panel_native_pane": "%71"}
            def cancel_as_main(*args, **kwargs):
                with patch.dict(os.environ, {"TMUX": "fixture", "TMUX_PANE": "%72",
                                              "OMS_PANEL_SESSION": "oms-fixture"}):
                    with patch.object(panel, "pane_option", side_effect=lambda key: watch_options.get(key, "")), \
                            patch.object(panel, "pane_local_option",
                                         side_effect=lambda key: watch_options.get(key, "")):
                        return panel.cancel_selected_call(*args, **kwargs)
            cancel_preflight = cancel_as_main(project, selected_state, selected_nav, main_attempt)
            main_fixture_row = json.loads(panel.events(project, "show", "--attempt", main_attempt,
                                                       "--json", output=True))
            assert cancel_preflight == "ready", (cancel_preflight, main_fixture_row)
            assert process.poll() is None and panel._dispatch_supervisor_receipt(project, call)["exit"] is None, \
                "cancellation preflight signalled before confirmation"
            tree = complete_descendants(process.pid)
            assert tree and not panel._dispatch_group_gone(process.pid), "long-running provider tree was not observed"
            foreign_nav = dict(selected_nav, cancel_owner={"participant": "foreign-main", "attempt": main_attempt})
            assert cancel_as_main(project, selected_state, foreign_nav, main_attempt) != "ready", \
                "foreign call cancellation was allowed"
            resumed_owner = "resumed-main"
            resumed_nav = dict(selected_nav, cancel_owner={"participant": resumed_owner, "attempt": main_attempt})
            resumed_state = room.status(project, shared)
            resumed_state["participants"] = [dict(row, participant=resumed_owner)
                if row["participant"] == room_owner else dict(row, parent=resumed_owner)
                if row["participant"] == name else row for row in resumed_state["participants"]]
            original_events = panel.events
            def resumed_events(repo, *args, **kwargs):
                if "--attempt" in args and args[args.index("--attempt") + 1] == main_attempt:
                    return json.dumps({"tool": "panel-main", "terminal": False,
                                       "refs": {"panel_room_id": shared,
                                                "panel_room_participant": resumed_owner}})
                return original_events(repo, *args, **kwargs)
            watch_options["@oms_panel_room_participant"] = resumed_owner
            with patch.object(panel.room, "status", return_value=resumed_state), \
                    patch.object(panel, "events", side_effect=resumed_events), \
                    patch.object(panel, "_dispatch_call_record", return_value=dict(call, parent=resumed_owner)), \
                    patch.object(panel, "_dispatch_supervisor_snapshot", return_value=(
                        panel._dispatch_supervisor_receipt(project, call),
                        panel.dispatch_store.read(project, name + "-process.json"))):
                assert cancel_as_main(project, selected_state, resumed_nav, main_attempt) == "ready", \
                    "resumed main participant was confused with its current attempt"
            watch_options["@oms_panel_room_participant"] = room_owner
            with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1"}):
                assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                    "child caller was allowed to cancel"
            with patch.dict(os.environ, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
                assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                    "delegate caller was allowed to cancel"
            with patch.dict(os.environ, {"OMS_PANEL_DISPATCH": "1", "OMS_PANEL_ROLE": "worker"}):
                assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                    "worker caller was allowed to cancel"
            watch_options["@oms_panel_main_attempt"] = "observer-attempt"
            assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                "observational watch was allowed to cancel"
            watch_options["@oms_panel_main_attempt"] = main_attempt
            watch_options["@oms_panel_main_watch"] = ""
            assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                "unmarked observer pane was allowed to cancel"
            watch_options["@oms_panel_main_watch"] = "1"
            binding = panel.dispatch_store.read(project, name + "-process.json")
            panel.dispatch_store.write(project, name + "-process.json",
                                       dict(binding, identity="different-process-generation"))
            assert cancel_as_main(project, selected_state, selected_nav, main_attempt, send=True).startswith(
                "Supervisor identity changed"), "reused supervisor PID was signalled"
            assert process.poll() is None, "identity mismatch still signalled the supervisor"
            panel.dispatch_store.write(project, name + "-process.json", dict(binding, pid=process.pid + 1))
            assert cancel_as_main(project, selected_state, selected_nav, main_attempt) != "ready", \
                "receipt/binding PID mismatch was accepted"
            panel.dispatch_store.write(project, name + "-process.json", binding)
            panel.dispatch_store.write(project, name + "-process.json", {"pid": binding["pid"]})
            assert cancel_as_main(project, selected_state, selected_nav, main_attempt).startswith(
                "Legacy supervisor binding cannot be safely cancelled"), \
                "legacy PID-only binding was allowed to cancel"
            assert process.poll() is None, "legacy PID-only binding still signalled the supervisor"
            panel.dispatch_store.write(project, name + "-process.json", binding)
            if hasattr(panel.os, "pidfd_open") and hasattr(panel.signal, "pidfd_send_signal"):
                cancel_result = cancel_as_main(project, selected_state, selected_nav, main_attempt, send=True)
            else:
                cancel_result = cancel_as_main(project, selected_state, selected_nav, main_attempt, send=True)
                assert cancel_result.startswith("Safe supervisor signalling is unavailable") and process.poll() is None, \
                    "platform without pidfd did not fail closed"
                cancel_result = "platform-no-pidfd"
            if cancel_result != "platform-no-pidfd":
                assert cancel_result.startswith("Cancellation requested"), "selected call was not signalled"
                assert process.wait(timeout=8) == 143, "supervisor did not receive cancellation"
                receipt = panel._dispatch_supervisor_receipt(project, call)
                assert receipt and receipt["all_reaped"] and receipt["exit"] == 143, "provider tree was not reaped"
                assert all(panel_terminals.process_gone(row["pid"], row["identity"]) for row in tree), \
                    "provider descendant survived supervisor cancellation"
                assert panel._dispatch_group_gone(process.pid), "supervisor process group survived cancellation"
                panel._record_dispatch_pending(project, call, 143, "cancel-selected-call",
                                               {"descendants": complete_descendants(process.pid)})
                panel._write(project, panel._dispatch_call_path(name, True), panel._dispatch_exit_record(call, 143))
                panel._settle_dispatch(project, shared, name, 143)
                panel.deliver_result(project, shared, name, room_owner, "cancel-selected-call", 143)
                settled = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                                  "--json", output=True))
                assert settled["terminal"] and settled["state"] == "cancelled", settled
                assert panel._dispatch_released(project, call), "settled room call record was not released"
                assert any(message["id"] == "result-" + name for message in room.status(project, shared)["messages"]), \
                    "settled call result was not recorded in the room"
                assert cancel_as_main(project, selected_state, selected_nav, main_attempt) == \
                    "Selected call is no longer running", "terminal ledger call was not reported as a no-op"

        with recorded_fixture("receipt-ports") as fixture:
            name, member, call, process = (fixture[k] for k in ("name", "member", "call", "process"))
            state = room.status(project, shared)
            repository_receipt = project / panel._dispatch_call_path(name, True)
            repository_pending = project / panel._dispatch_pending_path(name)
            repository_receipt.write_text(json.dumps(panel._dispatch_exit_record(call, 0)))
            assert re.fullmatch(r"[0-9a-f]{64}", fixture["digest"]), "receipt digest format lost"
            assert not panel._dispatch_call_finished(project, shared, member), "receipt released a live process"
            assert any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, state, room_owner, ["value.txt"])), "forged live receipt suppressed scope overlap"
            with patch.object(panel, "main_history", return_value=[dict(fixture["row"], terminal=True)]):
                assert any(row["participant"] == name for row in panel.scope_overlaps(
                    project, shared, state, room_owner, ["value.txt"])), "terminal attempt released unverified scope"
            with patch.object(panel_terminals, "process", return_value=None):
                assert not panel._dispatch_call_finished(project, shared, member), "receipt bypassed unknown leader identity"
            assert fixture["row"]["state"] == "working" and not fixture["row"]["terminal"], "live fixture was settled"
            assert name not in state["call_results"], "live call delivered a completion result"
            binding = panel.dispatch_store.read(project, name + "-process.json")
            assert binding["pid"] == process.pid, "controller did not bind the actual Popen PID"
            assert panel._dispatch_supervisor_receipt(project, call)["pid"] == process.pid, "supervisor receipt names another process"
            for descendants in (None, []):
                repository_pending.write_text(json.dumps(dict(call, exit=0, task_id="receipt-ports", forced=False,
                                                               descendants=descendants)))
                assert not panel._dispatch_call_finished(project, shared, member), "forged empty observation released scope"
            repository_pending.unlink()
            repository_pending.write_text(json.dumps(dict(call, exit=143, task_id="receipt-ports", forced=True, descendants=[])))
            assert not panel._dispatch_call_finished(project, shared, member), "new sibling pending artifact released scope"
            room.send(project, shared, name, room_owner, "Call exit=143; parent acceptance pending.",
                      kind="handoff", message_id="result-" + name)
            assert any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, room.status(project, shared), room_owner, ["value.txt"])), "premature handoff released live scope"
            with patch.dict(os.environ, room_env, clear=True):
                panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
            current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"], "--json", output=True))
            assert current["state"] == "working" and not current["terminal"], "live receipt reconciled"
            fixture["release"].touch()
            assert process.wait(timeout=5) == 0, "quick supervisor completion failed"
            receipt = panel._dispatch_supervisor_receipt(project, call)
            assert receipt["all_reaped"] and receipt["exit"] == 0, "supervisor did not attest child exhaustion"
            assert panel._dispatch_call_finished(project, shared, member), "confirmed termination kept scope"
            assert panel._dispatch_descendants(process.pid) == [], "absent leader prevented session observation"
            assert not any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, state, room_owner, ["value.txt"])), "verified proof kept completed scope"
            private = panel.dispatch_store.store(project)
            receipt_name = name + "-supervisor.json"
            for forged in (dict(receipt, launch_sha256="0" * 64),
                           {key: value for key, value in receipt.items() if key != "launch_sha256"},
                           dict(receipt, pid=process.pid + 1), dict(receipt, all_reaped=False), dict(receipt, exit=True)):
                panel.dispatch_store.write(project, receipt_name, forged)
                assert not panel._dispatch_call_finished(project, shared, member), "mismatched/legacy receipt accepted"
                assert any(row["participant"] == name for row in panel.scope_overlaps(
                    project, shared, state, room_owner, ["value.txt"])), "invalid proof suppressed scope overlap"
            panel.dispatch_store.write(project, receipt_name, receipt)
            assert panel._dispatch_call_finished(project, shared, member), "authentic gone descendants kept scope"
            panel.dispatch_store.write(project, name + "-process.json", {"pid": process.pid})
            assert panel._dispatch_supervisor_receipt(project, call)["pid"] == process.pid, \
                "legacy PID-only binding rejected a valid supervisor receipt"
            assert panel._dispatch_call_finished(project, shared, member), \
                "legacy PID-only binding prevented settlement of a valid completed call"
            pending_name = name + "-pending.json"
            pending = panel.dispatch_store.read(project, pending_name)
            for invalid in ({}, dict(pending, task_id="../foreign"), dict(pending, launch_sha256="0" * 64)):
                panel.dispatch_store.write(project, pending_name, invalid)
                assert not panel._dispatch_call_finished(project, shared, member), "receipt bypassed invalid pending evidence"
            (private / pending_name).unlink()
            assert not panel._dispatch_call_finished(project, shared, member), "missing observation released scope"
            assert any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, state, room_owner, ["value.txt"])), "missing observation suppressed scope overlap"
            panel.dispatch_store.write(project, pending_name, pending)
            repository_receipt.unlink()
            assert panel._dispatch_call_finished(project, shared, member), "independent exit evidence required a receipt"
            # A fresh controller accepts durable evidence, but never reconstructs authority from repository bytes.
            probe = ("import json,sys; from pathlib import Path; sys.path.insert(0,sys.argv[1]); "
                     "import terminal_panel as p; repo=Path(sys.argv[2]); m=json.loads(sys.argv[4]); "
                     "expected=sys.argv[5]=='yes'; "
                     "assert p._dispatch_call_finished(repo,sys.argv[3],m)==expected; "
                     "assert any(r['participant']==m['participant'] for r in "
                     "p.scope_overlaps(repo,sys.argv[3],{'participants':[m]},m['parent'],['value.txt']))!=expected")
            args = [sys.executable, "-c", probe, str(root / "scripts/lib"), str(project), shared, json.dumps(member)]
            fresh = subprocess.run(args + ["yes"], capture_output=True, text=True)
            assert fresh.returncode == 0, "completed scope reappeared in a new controller: " + fresh.stderr
            anchor_name = name + "-launch.json"
            anchor = panel.dispatch_store.read(project, anchor_name)
            (private / anchor_name).unlink()
            assert not panel._dispatch_call_finished(project, shared, member), "restart trusted repository evidence"
            fresh = subprocess.run(args + ["no"], capture_output=True, text=True)
            assert fresh.returncode == 0, "restart released unverified scope: " + fresh.stderr
            assert any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, state, room_owner, ["value.txt"])), "restart released unverified scope"
            panel.dispatch_store.write(project, anchor_name, anchor)
            assert panel._dispatch_call_finished(project, shared, member), "restored authentic proof kept scope"
            with recorded_fixture("receipt-replay") as other:
                panel.dispatch_store.write(project, other["name"] + "-supervisor.json", receipt)
                assert not panel._dispatch_call_finished(project, shared, other["member"]), "receipt replayed against another launch"
            launch_path = project / panel._dispatch_call_path(name)
            original_launch = launch_path.read_bytes()
            launch_path.write_text(json.dumps(dict(call, nonce="different-launch")))
            assert not panel._dispatch_call_finished(project, shared, member), "receipt replayed against a substituted launch"
            launch_path.write_bytes(original_launch)
            assert not panel._dispatch_call_finished(project, shared, member), "identical bytes repaired a changed launch"

        with recorded_fixture("legacy-binding-settlement") as fixture:
            name, call, process = (fixture[key] for key in ("name", "call", "process"))
            fixture["release"].touch()
            assert process.wait(timeout=5) == 0, "legacy-binding supervisor did not finish"
            panel._record_dispatch_pending(project, call, 0, "legacy-binding-settlement", {"descendants": []})
            panel.dispatch_store.write(project, name + "-process.json", {"pid": process.pid})
            # A prior dispatcher settled and exited before creating its delivery claim.
            panel._settle_dispatch(project, shared, name, 0)
            assert not (panel.dispatch_store.store(project) / panel._settlement_claim_path(name)).exists()
            with patch.dict(os.environ, room_env, clear=True):
                panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
            settled = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"],
                                              "--json", output=True))
            assert settled["terminal"] and settled["state"] == "failed", settled
            assert panel._dispatch_released(project, call), "legacy PID-only binding did not release its room record"
            recovered = panel.dispatch_store.read(project, panel._settlement_claim_path(name))
            assert recovered["complete"] and recovered["steps"]["room_result"] is True, recovered
            assert len([m for m in room.status(project, shared)["messages"]
                        if m["id"] == "result-" + name]) == 1
            assert len([row for row in room.records(project, shared)
                        if row.get("room_event", {}).get("kind") == "call-result"
                        and row["room_event"].get("call_id") == name]) == 1

        with recorded_fixture("legacy-tmp-records", member_id="call-reviewer") as fixture:
            name, call, process = (fixture[key] for key in ("name", "call", "process"))
            fixture["release"].touch()
            assert process.wait(timeout=5) == 0
            panel._record_dispatch_pending(project, call, 0, "legacy-tmp-records", {"descendants": []})
            legacy = (Path("/tmp") / ("oms-dispatch-" + str(os.getuid())) /
                      hashlib.sha256(str(project.resolve()).encode()).hexdigest())
            legacy.parent.mkdir(mode=0o700, exist_ok=True)
            legacy.mkdir(mode=0o700, exist_ok=True)
            saved = {}
            for suffix in ("launch", "process", "pending", "supervisor"):
                record_name = name + "-" + suffix + ".json"
                saved[record_name] = panel.dispatch_store.read(project, record_name)
                path = legacy / record_name
                with path.open("x") as stream:
                    json.dump(saved[record_name], stream)
                path.chmod(0o600)
            private = panel.dispatch_store.store(project) / (name + "-launch.json")
            supervisor_record = json.loads((private.parent / (name + "-supervisor.json")).read_text())
            assert supervisor_record["supervisor"]["pid"] == process.pid
            assert supervisor_record["supervisor"]["identity"] == fixture["supervisor"]["identity"]
            assert supervisor_record["writer"] == supervisor_record["supervisor"]
            private.parent.rename(temporary / (name + "-durable-archive"))
            try:
                assert panel._dispatch_call_finished(project, shared, fixture["member"]), "legacy /tmp call did not settle"
                panel._settle_dispatch(project, shared, name, 0)
                assert panel._dispatch_released(project, call), "legacy call settlement was not readable"
                durable_settlement = (Path(room_env["XDG_STATE_HOME"]) / "oh-my-setting/panel-control" /
                                      hashlib.sha256(str(project.resolve()).encode()).hexdigest() / "dispatch" /
                                      name / (name + "-settled.json"))
                assert durable_settlement.is_file(), "legacy settlement was not written to durable storage"
                assert not (legacy / (name + "-settled.json")).exists(), "legacy settlement modified the read-only store"
                assert not list(legacy.glob("settlement-*.json")), "settlement claim modified the legacy store"
                assert panel._dispatch_call_record(project, shared, fixture["member"]) is not None, "legacy call record disappeared"
                with patch.dict(os.environ, room_env, clear=True):
                    panel._reconcile_dispatch(project, shared, room.status(project, shared), room_owner, room_owner)
                delivered = room.status(project, shared)
                assert any(message["id"] == "result-" + name for message in delivered["messages"]), "legacy result was not delivered"
                claim_path = panel.dispatch_store.store(project) / panel._settlement_claim_path(name)
                assert claim_path.is_file(), "legacy delivery claim was not written durably"
                assert not list(legacy.glob("settlement-*.json")), "legacy store received a settlement claim"
                for record_name, record_value in saved.items():
                    assert panel.dispatch_store.read(project, record_name) == record_value, "legacy record changed: " + record_name
                assert panel._dispatch_call_record(project, shared, fixture["member"]) is not None, "legacy call record was hidden"
                pending_path = legacy / (name + "-pending.json")
                pending_path.unlink()
                assert not panel._dispatch_call_finished(project, shared, fixture["member"]), "missing legacy pending record proved exit"
            finally:
                for path in legacy.glob(name + "-*.json"):
                    path.unlink()

        already_delivered = "call-legacy-result-present"
        room.join(project, shared, already_delivered, "codex", "worker", parent=room_owner)
        room.send(project, shared, already_delivered, room_owner, "Prior result", "handoff",
                  message_id="result-" + already_delivered)
        assert not panel._settlement_delivery_pending(project, shared, already_delivered)

        # Session/group membership and snapshots cannot hide an adopted survivor or authorize release.
        for topology in ("group", "session", "shared"):
            detached = topology == "session"
            body = ("child_code=" + repr("import os,time\nfrom pathlib import Path\n" + atomic_ready) + " + '"
                    + ("os.setpgrp(); " if topology == "group" else "") +
                    "publish_ready(' + repr(ready) + ', str(os.getpid())); "
                    "\\nwhile not Path(' + repr(release) + ').exists(): time.sleep(.02)\\n'\n"
                    "subprocess.Popen([sys.executable,'-c',child_code],start_new_session=" + repr(detached) + ")\n"
                    "while not Path(ready).exists(): time.sleep(.01)\n")
            with recorded_fixture("survivor-" + topology, body) as fixture:
                name, member, call, process = (fixture[k] for k in ("name", "member", "call", "process"))
                deadline = time.monotonic() + 5
                while panel._dispatch_supervisor_receipt(project, call)["exit"] is None and time.monotonic() < deadline:
                    time.sleep(.02)
                receipt = panel._dispatch_supervisor_receipt(project, call)
                child = int(fixture["ready"].read_text())
                assert receipt["exit"] == 0 and not receipt["all_reaped"], "provider exit bypassed a surviving descendant"
                assert process.poll() is None, "supervisor abandoned a surviving descendant"
                assert panel_terminals.process(child)["parent"] == process.pid, "orphan was not adopted"
                assert (os.getsid(child) == process.pid) is (not detached), "session topology did not match fixture"
                assert (os.getpgid(child) == child) is (topology != "shared"), "detached group was not observed"
                if topology == "group":
                    assert os.getsid(child) == process.pid and os.getpgid(child) == child, "same-session group survivor escaped fixture"
                snapshots = [complete_descendants(process.pid) for unused in range(2)]
                assert len(snapshots) >= 2 and all(snapshots), snapshots
                snapshot = snapshots[-1]
                assert any(row["pid"] == child for row in snapshot), "snapshot missed the survivor"
                assert not panel._dispatch_descendants_gone(snapshot), "live descendant was called gone"
                evidence = {}
                with patch.object(panel, "_dispatch_descendants", side_effect=[snapshot, None, []]):
                    for unused in range(3):
                        panel._observe_dispatch_descendants(process, evidence)
                assert evidence["descendants"] == snapshot, "failed scan discarded recorded descendants"
                assert evidence["observation_failed"], "failed scan repaired itself from empty evidence"
                pending_status = {"group": 143, "session": 1, "shared": 0}[topology]
                panel._record_dispatch_pending(project, call, pending_status, "survivor-" + topology, evidence)
                assert panel._dispatch_pending(project, call)["descendants"] is None, "failed scan became complete pending evidence"
                with patch.object(panel, "_dispatch_group_gone", return_value=True), \
                        patch.object(panel, "_dispatch_session_gone", return_value=True):
                    assert not panel._dispatch_call_finished(project, shared, member), "receipt bypassed a surviving group"
                assert not panel._dispatch_call_finished(project, shared, member), "receipt bypassed a surviving descendant"
                state = room.status(project, shared)
                assert name not in state["call_results"] and not fixture["row"]["terminal"], "survivor received terminal settlement"
                assert any(row["participant"] == name for row in panel.scope_overlaps(
                    project, shared, state, room_owner, ["value.txt"])), "surviving descendant released scope"
                with patch.dict(os.environ, room_env, clear=True):
                    panel._reconcile_dispatch(project, shared, state, room_owner, room_owner)
                assert name not in room.status(project, shared)["call_results"], "live session reconciled"
                assert not panel._read(project, panel._dispatch_call_path(name, True), missing=True), "survivor received exit receipt"
                fixture["release"].touch()
                assert process.wait(timeout=5) == 0, "surviving child was not reaped"
                assert panel._dispatch_call_finished(project, shared, member), "gone descendants kept scope"
                assert panel._dispatch_descendants_gone(snapshot), "reaped descendants stayed live"
                # A whole-host process snapshot can be inconclusive during churn.
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    if panel._dispatch_session_gone(process.pid):
                        break
                    time.sleep(.02)
                else:
                    raise AssertionError("reaped session remained populated")
                next_status, next_member, next_notice = dispatch_case("pass", "after-survivor-" + topology)
                assert next_status == 0, "next dispatch failed after authentic descendant exit: " + next_notice
                current = json.loads(panel.events(project, "show", "--attempt", fixture["row"]["attempt_id"], "--json", output=True))
                after = room.status(project, shared)
                assert current["terminal"] and current["state"] == ("cancelled" if pending_status == 143 else "failed"), "reaped survivor did not reconcile"
                assert current["reason_code"] == "panel_call_interrupted", "reconciliation lost its reason code"
                assert after["call_results"][name]["exit"] == pending_status, "deferred status did not survive reconciliation"
                if topology == "shared":
                    assert current["terminal"] and after["call_results"][name]["exit"] == 0, "observed normal tree lost successful settlement"
                assert not any(row["participant"] == name for row in panel.scope_overlaps(
                    project, shared, after, room_owner, ["value.txt"])), "reaped survivor kept scope"
                assert panel._dispatch_released(project, call), "reconciliation did not persist release"
                assert room.participant(after, name)["joined"], "settlement changed participant identity"
                fresh = subprocess.run([sys.executable, "-c", probe, str(root / "scripts/lib"), str(project), shared,
                                        json.dumps(member), "yes"], capture_output=True, text=True)
                assert fresh.returncode == 0, "reconciled scope reappeared after restart: " + fresh.stderr
        after_subreaper = ctypes.c_int()
        assert libc.prctl(37, ctypes.byref(after_subreaper), 0, 0, 0) == 0, "parent subreaper state unreadable after calls"
        assert after_subreaper.value == previous.value, "per-call supervisors changed the controller's subreaper state"
    # Missing evidence and every failed observation stay unknown, including an earlier empty scan.
    assert not panel._dispatch_recorded_gone(None)
    evidence = {}
    with patch.object(panel, "_dispatch_descendants", side_effect=[[], None, []]):
        for unused in range(3):
            panel._observe_dispatch_descendants(Mock(pid=2147483647), evidence)
        assert evidence["descendants"] == [] and evidence["observation_failed"]
    # Assignment/launch refusal and Popen failure all roll back the unstarted enrollment.
    real_launch = subprocess.Popen
    refused_launches = []
    def refuse_provider(argv, *args, **kwargs):
        if "dispatch_supervisor.py" in " ".join(map(str, argv)) or argv[:3] == [sys.executable, "-c", "pass"]:
            refused_launches.append(argv)
            raise AssertionError("provider started after refused launch")
        return real_launch(argv, *args, **kwargs)
    for stage, failure in (("assignment", ValueError("assignment denied")),
                           ("launch", OSError("launch denied")),
                           ("launch", ValueError("launch substituted")),
                           ("spawn", OSError("spawn denied"))):
        real_send, real_prepare = room.send, panel._prepare_dispatch_call
        failed_spawns = []
        def send_assignment(*args, **kwargs):
            if stage == "assignment" and args[4].startswith("Assigned:"):
                try:
                    room.append(project, shared, {"kind": "leave", "participant": args[3]}, "Foreign leave")
                except ValueError as error:
                    assert "leave must name this session's participant and room" in str(error), error
                else:
                    raise AssertionError("parent bypassed the ordinary leave guard")
                raise failure
            return real_send(*args, **kwargs)
        def prepare_launch(*args, **kwargs):
            if stage == "launch":
                raise failure
            return real_prepare(*args, **kwargs)
        def launch_provider(argv, *args, **kwargs):
            if stage == "spawn" and ("dispatch_supervisor.py" in " ".join(map(str, argv))
                                     or argv[:3] == [sys.executable, "-c", "pass"]):
                failed_spawns.append(argv)
                raise failure
            return refuse_provider(argv, *args, **kwargs)
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel, "plan_link", return_value=None), \
                patch.object(room, "send", side_effect=send_assignment), \
                patch.object(panel, "_prepare_dispatch_call", side_effect=prepare_launch), \
                patch.object(panel.subprocess, "Popen", side_effect=launch_provider):
            before = {p["participant"] for p in room.status(project, shared)["participants"]}
            try:
                panel.run_dispatch(project, "claude", "worker", "read", "Inspect", None, None,
                                   "launch-refused", False, None, route, None, None, None)
            except (OSError, ValueError) as error:
                assert error is failure
            else:
                raise AssertionError("launch record failure started a provider")
            assert not refused_launches
            assert len(failed_spawns) == (1 if stage == "spawn" else 0)
            assert os.environ["OMS_ROOM_PARTICIPANT"] == room_owner, "rollback changed the caller binding"
            added = [p for p in room.status(project, shared)["participants"] if p["participant"] not in before]
            assert len(added) == 1 and not added[0]["joined"]
            refused = added[0]["participant"]
            assert refused not in room.status(project, shared)["call_results"], "refused launch delivered a result"
            assert not panel._read(project, panel._dispatch_pending_path(refused), missing=True), "refused launch retained pending evidence"
            if stage != "spawn":
                assert not panel._dispatch_call_record(project, shared, added[0]), "refused launch left trusted call evidence"
            else:
                assert panel._dispatch_call_record(project, shared, added[0]), "spawn failure lost launch diagnostics"
                assert not (panel.dispatch_store.store(project) / (refused + "-process.json")).exists(), "failed spawn bound a process"
    # Binding cleanup to a child must not let a harness worker leave a room.
    with patch.dict(os.environ, dict(room_env, OMS_HARNESS_CHILD="1"), clear=True):
        try:
            panel._rollback_dispatch_enrollment(project, shared, room_owner)
        except ValueError as error:
            assert "a worker can only message or acknowledge" in str(error), error
        else:
            raise AssertionError("worker used controller rollback to leave the room")
    assert room.participant(room.status(project, shared), room_owner)["joined"]
    member = "call-" + "e" * 16
    precreated = project / panel._dispatch_call_path(member)
    precreated.write_text('{"pid":2147483647}')
    try:
        panel._prepare_dispatch_call(project, shared, member, room_owner)
    except ValueError:
        pass
    else:
        raise AssertionError("precreated launch record accepted")
    precreated.unlink()
    if sys.platform == "linux":
        status, member, notice = dispatch_case("pass", "subreaper-quick")
        assert status == 0, notice
        name = member["participant"]
        assert room.status(project, shared)["call_results"][name]["exit"] == 0, "quick call lost its successful result"
        call = panel._dispatch_call_record(project, shared, member)
        assert call and panel._dispatch_call_finished(project, shared, member)
        assert not room.participant(room.status(project, shared), name)["owns"]
        # Fresh interpreter checks durable receipt, including a stale projected scope.
        probe = ('import sys,json; from pathlib import Path; sys.path.insert(0,sys.argv[1]); '
                 'import terminal_panel as p; '
                 'assert p._dispatch_call_finished(Path(sys.argv[2]),sys.argv[3],json.loads(sys.argv[4]))')
        fresh = subprocess.run([sys.executable, "-c", probe, str(root / "scripts/lib"), str(project), shared,
                                json.dumps(member)], capture_output=True, text=True)
        assert fresh.returncode == 0, fresh.stderr
        launch_path = project / panel._dispatch_call_path(name)
        assert stat.S_IMODE(launch_path.stat().st_mode) == 0o600
        original = launch_path.read_bytes()
        launch_path.write_text(json.dumps(dict(call, pid=2147483647)))
        assert not panel._dispatch_call_finished(project, shared, member), "substituted launch released scope"
        # Restoring bytes cannot undo the generation change.
        launch_path.write_bytes(original)
        assert not panel._dispatch_call_finished(project, shared, member)

        status, member, notice = dispatch_case("pass", "subreaper-missing")
        assert status == 0, notice
        call = panel._dispatch_call_record(project, shared, member)
        private = panel.dispatch_store.store(project)
        pending_path = private / (member["participant"] + "-pending.json")
        pending = pending_path.read_bytes()
        pending_path.unlink()
        assert not panel._dispatch_call_finished(project, shared, member)
        # A repository receipt/pending cannot replace missing controller evidence.
        (project / panel._dispatch_pending_path(member["participant"])).write_text(json.dumps(dict(call, descendants=[])))
        assert not panel._dispatch_call_finished(project, shared, member)
        pending_path.write_bytes(pending)
        pending_path.chmod(0o600)
        assert panel._dispatch_call_finished(project, shared, member)

        # Controller TERM reaches the supervised provider and keeps reaping proof alive.
        import threading
        ready = temporary / "supervised-term.ready"
        cleaned = temporary / "supervised-term.cleaned"
        cancel_code = ("import signal,subprocess,sys,time; from pathlib import Path\n"
                       "def cleanup(n,f):\n"
                       "    subprocess.run([sys.executable,'-c','import time; time.sleep(.3)'],check=True)\n"
                       "    Path(" + repr(str(cleaned)) + ").touch()\n"
                       "    sys.exit(0)\n"
                       "signal.signal(signal.SIGTERM,cleanup)\n"
                       "Path(" + repr(str(ready)) + ").touch(); time.sleep(60)")
        def cancel_when_ready():
            deadline = time.monotonic() + 10
            while not ready.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            if ready.exists():
                os.kill(os.getpid(), signal.SIGTERM)
        cancellation = threading.Thread(target=cancel_when_ready)
        cancellation.start()
        try:
            dispatch_case(cancel_code, "subreaper-term")
        except panel.NativeShutdown as error:
            assert error.exit_code == 143
        else:
            raise AssertionError("controller TERM was swallowed")
        finally:
            cancellation.join(timeout=12)
        assert not cancellation.is_alive() and cleaned.exists(), "TERM interrupted provider cleanup"
        stopped = next(p for p in reversed(room.status(project, shared)["participants"])
                       if p.get("label") == "investigate")
        assert panel._dispatch_call_finished(project, shared, stopped), "TERM destroyed exhaustion proof"
        assert room.status(project, shared)["call_results"][stopped["participant"]]["exit"] == 143

        # A double fork creates an orphan in a new session between observations.
        pid_file, release_file = temporary / "subreaper-orphan.pid", temporary / "subreaper-release"
        code = ('''import os,time
from pathlib import Path
''' + atomic_ready + '''if os.fork() == 0:
    os.setsid()
    if os.fork() == 0:
        publish_ready(PID, str(os.getpid()))
        while not Path(RELEASE).exists(): time.sleep(.02)
        os._exit(0)
    os._exit(0)
os.wait()
while not Path(PID).exists(): time.sleep(.01)
''').replace("PID", repr(str(pid_file))).replace("RELEASE", repr(str(release_file)))
        try:
            with patch.object(panel, "_dispatch_descendants", return_value=None):
                status, member, notice = dispatch_case(code, "subreaper-orphan")
            assert status == 1 and "settlement pending" in notice and "Manual clear" in notice, notice
            name = member["participant"]
            call = panel._dispatch_call_record(project, shared, member)
            receipt = panel._dispatch_supervisor_receipt(project, call)
            orphan = int(pid_file.read_text())
            assert os.getsid(orphan) != receipt["pid"]
            assert panel_terminals.process(orphan)["parent"] == receipt["pid"], "orphan escaped subreaper"
            room.scope(project, shared, name, ["value.txt"])
            assert not panel._dispatch_call_finished(project, shared, member)
            assert any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, room.status(project, shared), room_owner, ["value.txt"]))
            release_file.touch()
            deadline = time.monotonic() + 5
            while not panel._dispatch_call_finished(project, shared, member) and time.monotonic() < deadline:
                time.sleep(.05)
            assert panel._dispatch_call_finished(project, shared, member), "reaped orphan remained pending"
            assert panel_terminals.process_gone(orphan, None) or not Path('/proc/' + str(orphan)).exists()
            panel._settle_dispatch(project, shared, name, 1)
            assert panel._dispatch_released(project, call)
            assert not any(row["participant"] == name for row in panel.scope_overlaps(
                project, shared, room.status(project, shared), room_owner, ["value.txt"]))
            fresh = subprocess.run([sys.executable, "-c", probe, str(root / "scripts/lib"), str(project), shared,
                                    json.dumps(member)], capture_output=True, text=True)
            assert fresh.returncode == 0, fresh.stderr
        finally:
            release_file.touch()
    # Unsupported backends cannot claim whole-tree termination from a leader or snapshot.
    for platform in ("darwin", "win32", "freebsd"):
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel, "plan_link", return_value=None), \
                patch.object(panel.sys, "platform", platform), redirect_stderr(io.StringIO()) as notice:
            before = {p["participant"] for p in room.status(project, shared)["participants"]}
            assert panel.run_dispatch(project, "claude", "worker", "read", "Inspect", None, None,
                                      "unsupported", False, None, route, None, None, None) == 1
            member = next(p for p in room.status(project, shared)["participants"] if p["participant"] not in before)
            with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": member["participant"]}):
                room.scope(project, shared, member["participant"], ["value.txt"])
            assert not panel._dispatch_call_finished(project, shared, member)
            assert "Manual clear" in notice.getvalue() and "--clear" in notice.getvalue()
    # Each negative case has finished; isolate the menu's all-scope release contract.
    for row in room.status(project, shared)["participants"]:
        if row["participant"] not in original_members and row["joined"]:
            room.append(project, shared, {"kind": "leave", "participant": row["participant"]}, "Negative fixture complete")
    # Exercise the real child, handlers and ledger without a provider call: leave an active attempt on TERM.
    interrupted_file = temporary / "interrupted-attempt"
    cleaned_file = temporary / "interrupted-cleaned"
    interrupted_child = temporary / "interrupted-child.py"
    interrupted_child.write_text('''import os, signal, sys, time
from pathlib import Path
sys.path.insert(0, sys.argv[1])
import terminal_panel as panel
import room
def stop(number, frame):
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    time.sleep(0.1)
    Path(sys.argv[3]).write_text("cleaned")
    sys.exit(143)
signal.signal(signal.SIGTERM, stop)
repo, ident, member = Path(os.environ["OMS_ROOM_REPO"]), os.environ["OMS_ROOM_ID"], os.environ["OMS_ROOM_PARTICIPANT"]
room.scope(repo, ident, member, ["value.txt"])
attempt = panel.events(repo, "start", "--provider", "codex", "--tool", "peer-delegate", "--task-id", "interrupted-task",
                       "--parent-attempt-id", os.environ["OMS_PANEL_MAIN_ATTEMPT"],
                       "--ref", "panel_role=worker", "--ref", "panel_room_id=" + ident,
                       "--ref", "panel_room_participant=" + member, "--then", "starting", "--then", "working", output=True)
Path(sys.argv[2]).write_text(attempt)
os.kill(int(sys.argv[4]), signal.SIGTERM)
time.sleep(60)
''')
    menu_backends = ("linux", "darwin") if sys.platform == "linux" else (sys.platform,)
    for menu_backend in menu_backends:
        cleaned_file.unlink(missing_ok=True)
        interrupted_file.unlink(missing_ok=True)
        saved = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)}
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.sys, "platform", menu_backend), \
                redirect_stderr(io.StringIO()) as menu_notices, \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, str(interrupted_child),
                             str(root / "scripts/lib"), str(interrupted_file), str(cleaned_file), str(os.getpid()), "--repo", str(project)]):
            interrupted_route = allocate("claude", "worker", "light", "read", "investigate", {"codex"})
            def menu_dispatch(*args, **kwargs):
                return panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                          "interrupted-task", False, "Interrupted call", interrupted_route,
                                          None, None, None)
            with patch.object(panel, "dispatch", side_effect=menu_dispatch), \
                    patch.object(panel, "managed_session", return_value=False), \
                    patch.object(panel, "panel_mains", return_value=["claude"]), \
                    patch.object(panel, "raw_keys", return_value=False), \
                    patch.object(panel, "read_choice", return_value="5") as menu_choice, \
                    patch.object(panel, "read_field", side_effect=["Inspect source", "light"]) as menu_fields:
                try:
                    panel.interactive(project)
                except panel.NativeShutdown as error:
                    assert error.exit_code == 143
                else:
                    raise AssertionError("dispatch menu swallowed TERM after settlement")
                assert menu_choice.call_count == 1 and menu_fields.call_count == 2, "menu paused after TERM"
        assert all(signal.getsignal(number) == handler for number, handler in saved.items())
        interrupted = json.loads(panel.events(project, "show", "--attempt", interrupted_file.read_text(), "--json", output=True))
        member = interrupted["refs"]["panel_room_participant"]
        settled = room.status(project, shared)
        if menu_backend == "linux":
            assert interrupted["state"] == "cancelled" and interrupted["reason_code"] == "panel_call_interrupted", interrupted
            assert cleaned_file.read_text() == "cleaned" and settled["call_results"][member]["exit"] == 143
            assert room.participant(settled, member)["joined"] and panel.scope_overlaps(project, shared, settled, room_owner, ["value.txt"]) == []
            with patch.object(panel, "main_history", return_value=[dict(interrupted, state="review", terminal=False)]):
                assert panel.scope_overlaps(project, shared, settled, room_owner, ["value.txt"]) == [], "verified exit receipt did not release scope"
        else:
            assert cleaned_file.read_text() == "cleaned", "unsupported backend interrupted provider cleanup"
            assert interrupted["state"] == "working" and not interrupted["terminal"], "unsupported menu settled without tree proof"
            assert member not in settled["call_results"], "unsupported menu delivered a final result"
            assert "settlement pending" in menu_notices.getvalue() and "Manual clear" in menu_notices.getvalue()
            assert room.participant(settled, member)["joined"] and any(row["participant"] == member for row in
                panel.scope_overlaps(project, shared, settled, room_owner, ["value.txt"])), "unsupported menu released scope"
            with patch.object(panel, "main_history", return_value=[dict(interrupted, state="review", terminal=False)]):
                assert any(row["participant"] == member for row in panel.scope_overlaps(
                    project, shared, settled, room_owner, ["value.txt"])), "unsupported stale projection released scope"
            panel.events(project, "transition", "--attempt", interrupted["attempt_id"], "--state", "cancelled",
                         "--reason-code", "fixture_complete")
    # HUP follows TERM out of run_dispatch; the turn-interrupt paths retain exit 130.
    for interruption in (panel.NativeShutdown(signal.SIGHUP), panel.NativeShutdown(signal.SIGINT), KeyboardInterrupt()):
        finished = Mock(pid=2147483647, poll=lambda: 0)
        finished.wait.side_effect = interruption
        with patch.dict(os.environ, {}, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel, "plan_link", return_value=None), \
                patch.object(panel.subprocess, "Popen", return_value=finished), \
                patch.object(panel_terminals, "process", return_value=None), \
                patch.object(panel, "_dispatch_descendants", return_value=[]), \
                patch.object(panel, "_dispatch_group_gone", return_value=True):
            try:
                code = panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                          "interrupt-contract", False, None, interrupted_route, None, None, None)
            except panel.NativeShutdown as error:
                assert interruption is error and error.exit_code == 129
            else:
                assert code == 130
    # Keep direct teardown safety coverage below; it assists cancellation, never scope release.
    # Observe a real provider in a new session, with its parent reaping it during fixture cleanup.
    subtree_file = temporary / "provider-subtree.pid"
    subtree_child = temporary / "provider-subtree.py"
    subtree_child.write_text('''import os, signal, subprocess, sys, time
from pathlib import Path
''' + atomic_ready + '''child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"], start_new_session=True)
def stop(number, frame):
    child.terminate()
    child.wait(timeout=5)
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
publish_ready(sys.argv[1], str(child.pid))
time.sleep(60)
''')
    subtree = subprocess.Popen([sys.executable, str(subtree_child), str(subtree_file)], start_new_session=True)
    try:
        deadline = time.monotonic() + 5
        while not subtree_file.exists() and subtree.poll() is None and time.monotonic() < deadline:
            time.sleep(.02)
        detached = int(subtree_file.read_text())
        snapshot = complete_descendants(subtree.pid)
        assert snapshot and any(row["pid"] == row["group"] == detached for row in snapshot), snapshot
        assert all(panel_terminals.matches(row["pid"], row["identity"]) for row in snapshot)
        assert not panel._dispatch_descendants_gone(snapshot)
        if sys.platform == "linux":
            captured = None
            for unused in range(3):
                try:
                    captured = panel._dispatch_process_snapshot()
                except (OSError, ValueError, IndexError, KeyError):
                    continue
                if captured is not None:
                    break
            assert captured is not None, "process snapshot unavailable"
            members = {subtree.pid} | {pid for pid, row in captured.items() if row["session"] == subtree.pid}
            while True:
                expanded = members | {pid for pid, row in captured.items() if row["parent"] in members}
                if expanded == members:
                    break
                members = expanded
            expected = [{key: captured[pid][key] for key in ("pid", "identity", "group")}
                        for pid in sorted(members - {subtree.pid})]
            assert any(row["pid"] == detached for row in expected), expected
            with patch.object(panel, "_dispatch_process_snapshot", return_value=captured), \
                    patch.object(panel_terminals, "process", side_effect=AssertionError("identity re-read")):
                assert panel._dispatch_descendants(subtree.pid) == expected, "snapshot required a later identity read"
    finally:
        subtree.terminate()
        subtree.wait(timeout=5)
    assert panel._dispatch_descendants_gone(snapshot)
    if sys.platform == "linux":
        with patch.object(panel, "_dispatch_process_snapshot", side_effect=PermissionError):
            assert panel._dispatch_descendants(os.getpid()) is None, "unreadable process observation accepted"
            assert not panel._dispatch_session_gone(2147483647), "unreadable session observation accepted"
    with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(
            [], 0, "123 1\n456 1\n", "")), \
            patch.object(panel.sys, "platform", "darwin"), \
            patch.object(panel.os, "getsid", return_value=123), \
            patch.object(panel_terminals, "process", return_value={
                "pid": 456, "parent": 1, "identity": "session-member", "group": 456}):
        assert panel._dispatch_descendants(123) == [
            {"pid": 456, "identity": "session-member", "group": 456}], "reparented session member was missed"
        assert not panel._dispatch_session_gone(123), "portable session scan missed a member"
    with patch.object(panel.sys, "platform", "darwin"), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "456\r\n", "")), \
            patch.object(panel.os, "getsid", return_value=456):
        assert panel._dispatch_session_gone(123), "portable empty session was refused"
        with patch.object(panel.os, "getsid", side_effect=PermissionError):
            assert not panel._dispatch_session_gone(123), "unreadable portable session was accepted"
        with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, "", "")):
            assert not panel._dispatch_session_gone(123), "failed portable process table was accepted"
    provider = {"pid": 2147483646, "identity": "provider-identity", "group": 2147483646}
    evidence = {}
    with patch.object(panel, "_dispatch_descendants", side_effect=[[provider], None, []]):
        for unused in range(3):
            panel._observe_dispatch_descendants(Mock(pid=2147483647), evidence)
            assert evidence["descendants"] == [provider], "failed/later scan forgot a recorded child"
            assert bool(evidence.get("observation_failed")) == (unused > 0)
    for pid in (None, 0, -1, True):
        assert not panel._dispatch_session_gone(pid) and not panel._dispatch_leader_gone(pid, None)
    # An advisor-shaped shell exits on TERM while its foreground child and a
    # provider in another process group survive. Reaping that shell is not teardown.
    advisor_pids = temporary / "advisor-subtree.json"
    advisor_child = temporary / "advisor-subtree.py"
    advisor_child.write_text('''import json, os, signal, subprocess, sys, time
from pathlib import Path
''' + atomic_ready + '''signal.signal(signal.SIGTERM, signal.SIG_IGN)
child = subprocess.Popen([sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)"],
                         start_new_session=True)
publish_ready(sys.argv[1], json.dumps([os.getpid(), child.pid]))
time.sleep(60)
''')
    parent_group = os.getpgrp()
    parent_identity = panel_terminals.process(os.getpid())["identity"]
    real_killpg = os.killpg
    def owned_killpg(group, number):
        assert type(group) is int and group > 0 and (number == 0 or group != parent_group), "teardown targeted caller group"
        return real_killpg(group, number)
    sentinel = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
    try:
        for shares_group in (False, True):
            advisor_pids = temporary / ("advisor-subtree-%s.json" % shares_group)
            advisor = subprocess.Popen(["bash", "-c", "trap ':' EXIT; \"$@\"", "advisor",
                                        sys.executable, str(advisor_child), str(advisor_pids)], start_new_session=not shares_group)
            advisor_records = []
            try:
                deadline = time.monotonic() + 5
                while not advisor_pids.exists() and advisor.poll() is None and time.monotonic() < deadline:
                    time.sleep(.02)
                child_pids = json.loads(advisor_pids.read_text())
                advisor_identity = panel_terminals.process(advisor.pid)["identity"]
                advisor_records = complete_descendants(advisor.pid)
                assert advisor_records and {row["pid"] for row in advisor_records} >= set(child_pids), advisor_records
                evidence = {"descendants": advisor_records}
                panel._observe_dispatch_session(advisor, advisor_identity, evidence)
                advisor.terminate()
                advisor.wait(timeout=5)
                before_force = Mock(side_effect=ValueError("dispatch process record unavailable"))
                with patch.object(panel.os, "killpg", side_effect=owned_killpg), redirect_stderr(io.StringIO()) as notices:
                    verified = panel._stop_dispatch(advisor, advisor_identity, term_sent=True,
                                                    evidence=evidence, before_force=before_force)
                before_force.assert_called_once_with()
                assert "pending record unavailable" in notices.getvalue()
                assert panel_terminals.matches(os.getpid(), parent_identity) and os.getpgrp() == parent_group, "advisor teardown reached parent"
                assert sentinel.poll() is None, "advisor teardown killed caller-group sibling"
                assert all(not panel_terminals.matches(row["pid"], row["identity"]) for row in advisor_records), "advisor subtree survived"
                assert verified == (panel._dispatch_group_gone(advisor.pid)
                                    and panel._dispatch_descendants_gone(advisor_records)), "unverified advisor exit settled"
            finally:
                for row in advisor_records:
                    if panel_terminals.matches(row["pid"], row["identity"]):
                        os.kill(row["pid"], signal.SIGKILL)
                if advisor.poll() is None:
                    advisor.kill()
                advisor.wait(timeout=5)
    finally:
        sentinel.terminate()
        sentinel.wait(timeout=5)
    # Pending-record failures must not leave a TERM-resistant child running.
    for failure in (ValueError, OSError):
        ready = temporary / ("force-ready-" + failure.__name__)
        resistant = subprocess.Popen([sys.executable, "-c",
            "import signal,sys,time; from pathlib import Path; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            "Path(sys.argv[1]).write_text('ready'); time.sleep(60)", str(ready)],
            start_new_session=True)
        try:
            deadline = time.monotonic() + 5
            while not ready.exists() and resistant.poll() is None and time.monotonic() < deadline:
                time.sleep(.02)
            assert ready.exists(), "TERM-resistant child did not become ready"
            identity = panel_terminals.process(resistant.pid)["identity"]
            wait = resistant.wait
            def bounded_wait(timeout):
                return wait(timeout=.1 if timeout == 20 else timeout)
            before_force = Mock(side_effect=failure("dispatch process record unavailable"))
            with patch.object(resistant, "wait", side_effect=bounded_wait), \
                    redirect_stderr(io.StringIO()) as notices:
                assert panel._stop_dispatch(resistant, identity, before_force=before_force), \
                    "TERM-resistant dispatch was not stopped; notices=%r" % notices.getvalue()
            before_force.assert_called_once_with()
            assert resistant.poll() == -signal.SIGKILL
            assert panel_terminals.process_gone(resistant.pid, identity)
            assert "pending record unavailable" in notices.getvalue()
        finally:
            if resistant.poll() is None:
                resistant.kill()
            resistant.wait(timeout=5)
        portable = Mock(pid=123, poll=lambda: None)
        portable.wait.side_effect = [subprocess.TimeoutExpired("fixture", 20), 0]
        before_force = Mock(side_effect=failure("dispatch process record unavailable"))
        with patch.object(panel.os, "name", "nt"), \
                patch.object(panel, "_dispatch_exited", return_value=True), \
                patch.object(panel, "_dispatch_descendants_gone", return_value=True), \
                patch.object(panel, "_observe_dispatch_descendants"), \
                redirect_stderr(io.StringIO()) as notices:
            assert not panel._stop_dispatch(portable, None, before_force=before_force)
        before_force.assert_called_once_with()
        portable.kill.assert_called_once_with()
        assert "pending record unavailable" in notices.getvalue()
    # Normal and graceful exits need the same complete recorded tree evidence as forced exits.
    provider = {"pid": 2147483646, "identity": "provider-identity", "group": 2147483646}
    for already_reaped in (True, False):
        for descendants, gone in (([provider], False), ([provider], True), (None, True)):
            stopped = Mock(pid=2147483647, poll=lambda: 0 if already_reaped else None)
            stopped.wait.return_value = 0
            with patch.object(panel_terminals, "process_gone", return_value=gone), \
                    patch.object(panel_terminals, "matches", return_value=False), \
                    patch.object(panel, "_dispatch_group_gone", return_value=True):
                assert panel._stop_dispatch(stopped, "original-identity", term_sent=True,
                                            evidence={"descendants": descendants}) is (gone and descendants is not None)
            assert stopped.wait.call_count == int(not already_reaped)
    # Force-stop follows the grace; signaling alone is not evidence that a process group exited.
    for identity_matches, descendants in ((True, []), (False, []), (True, None)):
        stopped = Mock(pid=123, poll=lambda: None)
        stopped.wait.side_effect = [subprocess.TimeoutExpired("fixture", 20), 0]
        with patch.object(panel, "_dispatch_descendants", return_value=descendants), \
                patch.object(panel_terminals, "process", return_value={"group": 123}), \
                patch.object(panel.os, "getsid", return_value=123), \
                patch.object(panel.os, "getpgrp", return_value=999), \
                patch.object(panel_terminals, "matches", return_value=identity_matches), \
                patch.object(panel_terminals, "process_gone", return_value=False), \
                patch.object(panel, "_dispatch_group_gone", return_value=True), \
                patch.object(panel, "_dispatch_session_gone", return_value=descendants is not None), \
                patch.object(panel.os, "killpg") as force, redirect_stderr(io.StringIO()):
            assert panel._stop_dispatch(stopped, "original-identity") is (identity_matches and descendants is not None)
        assert stopped.terminate.call_count == 1 and stopped.wait.call_args_list[0].kwargs == {"timeout": 20}
        assert force.call_count == int(identity_matches)
    disappeared = Mock(pid=123, poll=lambda: None)
    disappeared.terminate.side_effect = ProcessLookupError
    with patch.object(panel_terminals, "process_gone", return_value=True), \
            patch.object(panel, "_dispatch_descendants", return_value=[]), \
            patch.object(panel, "_dispatch_group_gone", return_value=True):
        assert panel._stop_dispatch(disappeared, "original-identity", evidence={"descendants": []}) is True
    disappeared.wait.assert_not_called()
    # A dead leader bypasses the wait timeout. Still escalate its surviving group,
    # and signal a detached group only while the recorded process identity matches.
    for already_reaped, descendant_matches, current_group in (
            (False, True, 456), (False, False, 456), (False, True, 789),
            (True, True, 456), (True, False, 456), (True, True, 789)):
        stopped = Mock(pid=123, poll=lambda: 0 if already_reaped else None)
        stopped.wait.return_value = -signal.SIGTERM
        provider = {"pid": 456, "identity": "provider-identity", "group": 456}
        with patch.object(panel, "_dispatch_descendants", return_value=[provider]), \
                patch.object(panel_terminals, "process", return_value=None), \
                patch.object(panel.os, "getsid", return_value=456), \
                patch.object(panel.os, "getpgrp", return_value=999), \
                patch.object(panel_terminals, "matches", return_value=descendant_matches), \
                patch.object(panel_terminals, "process_gone", return_value=False), \
                patch.object(panel, "_dispatch_group_gone", return_value=False), \
                patch.object(panel.os, "getpgid", return_value=current_group), \
                patch.object(panel.time, "monotonic", side_effect=range(0, 100, 6)), \
                patch.object(panel, "_dispatch_session_gone", return_value=True), \
                patch.object(panel.os, "killpg") as force:
            evidence = {"session_leader": (123, "original-identity")}
            if already_reaped:
                evidence["descendants"] = [provider]
            assert panel._stop_dispatch(stopped, "original-identity", evidence=evidence, interrupted=True) is False
        expected = {(123, signal.SIGTERM), (123, signal.SIGKILL)}
        if descendant_matches and current_group == 456:
            expected.update({(456, signal.SIGTERM), (456, signal.SIGKILL)})
        assert {tuple(call.args) for call in force.call_args_list} == expected, force.call_args_list
        assert evidence["forced"] and evidence["descendants"] == [provider]
    # Port the seven pending lifecycle regressions: a dead leader, incomplete scan,
    # unconfirmed kill and graceful provider exit cannot replace exhaustion proof.
    real_popen = subprocess.Popen
    normal_scenarios = {"leader-only", "normal-provider-alive", "normal-unobserved"}
    provider_scenarios = {"provider-alive", "normal-provider-alive", "graceful-provider-alive"}
    for scenario in ("unknown-identity", "kill-unconfirmed", "provider-alive", "leader-only",
                     "normal-provider-alive", "normal-unobserved", "graceful-provider-alive"):
        pending = Mock(pid=2147483647, poll=lambda: 0 if scenario in normal_scenarios else None)
        pending.wait.side_effect = ([0] if scenario in normal_scenarios else
                                   [panel.NativeShutdown(signal.SIGTERM), subprocess.TimeoutExpired("fixture", 25)] +
                                   ([subprocess.TimeoutExpired("fixture", 5)] if scenario == "kill-unconfirmed" else
                                    [-signal.SIGKILL] if scenario == "provider-alive" else []))
        if scenario == "graceful-provider-alive":
            pending.wait.side_effect = [panel.NativeShutdown(signal.SIGTERM), 0]
        if scenario in {"normal-provider-alive", "normal-unobserved"}:
            pending.wait.side_effect = [subprocess.TimeoutExpired("fixture", 1), 0]
        pending_attempt = []
        def pending_child(argv, *args, **kwargs):
            if "dispatch_supervisor.py" not in " ".join(map(str, argv)):
                return real_popen(argv, *args, **kwargs)
            env = kwargs["env"]
            member = env["OMS_ROOM_PARTICIPANT"]
            with patch.dict(os.environ, env, clear=True):
                room.scope(project, shared, member, ["value.txt"])
                pending_attempt.append(panel.events(project, "start", "--provider", "codex", "--tool", "peer-delegate",
                    "--task-id", scenario, "--parent-attempt-id", room_owner,
                    "--ref", "panel_role=worker", "--ref", "panel_room_id=" + shared,
                    "--ref", "panel_room_participant=" + member, "--then", "starting", "--then", "working", output=True))
            return pending
        observation = ({"identity": "original-identity", "group": pending.pid, "state": "S"}
                       if scenario != "unknown-identity" else None)
        provider = {"pid": 2147483646, "identity": "provider-identity", "group": 2147483646}
        with patch.dict(os.environ, room_env, clear=True), \
                patch.object(panel.shutil, "which", return_value=sys.executable), \
                patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
                patch.object(panel.subprocess, "Popen", side_effect=pending_child), \
                patch.object(panel_terminals, "process", return_value=observation), \
                patch.object(panel_terminals, "process_gone", side_effect=lambda pid, identity: scenario in provider_scenarios and pid == pending.pid), \
                patch.object(panel_terminals, "matches", return_value=True), \
                patch.object(panel, "_dispatch_descendants",
                             side_effect=([[], None] if scenario == "normal-unobserved" else
                                          [[provider], []] if scenario == "normal-provider-alive" else None),
                             return_value=[provider] if scenario in provider_scenarios else []), \
                patch.object(panel, "_dispatch_group_gone", side_effect=lambda pid: (scenario in provider_scenarios or scenario == "normal-unobserved") and pid == pending.pid), \
                patch.object(panel, "_dispatch_session_gone", return_value=scenario != "normal-unobserved"), \
                patch.object(panel.os, "getpgid", side_effect=lambda pid: pid), \
                patch.object(panel.os, "getsid", side_effect=lambda pid: pid), \
                patch.object(panel.os, "getpgrp", return_value=999), \
                patch.object(panel.os, "killpg") as force, redirect_stderr(io.StringIO()) as notices:
            try:
                status = panel.run_dispatch(project, "claude", "worker", "read", "Inspect source", None, None,
                                            scenario, False, "Pending child", interrupted_route, None, None, None)
            except panel.NativeShutdown as error:
                assert scenario not in normal_scenarios and error.exit_code == 143
                status = error.exit_code
            else:
                assert scenario in normal_scenarios, "dispatch swallowed shutdown"
            row = json.loads(panel.events(project, "show", "--attempt", pending_attempt[0], "--json", output=True))
            member = row["refs"]["panel_room_participant"]
            state = room.status(project, shared)
            assert row["state"] == "working" and not row["terminal"]
            assert status == (1 if scenario in normal_scenarios else 143), (scenario, row)
            assert member not in state["call_results"] and "settlement pending" in notices.getvalue()
            assert not panel._read(project, panel._dispatch_call_path(member, True), missing=True)
            record = panel._dispatch_call_record(project, shared, room.participant(state, member))
            binding = panel.dispatch_store.read(project, member + "-process.json")
            assert binding["pid"] == pending.pid, "pending call bound another process"
            assert any(o["participant"] == member for o in panel.scope_overlaps(project, shared, state, room_owner, ["value.txt"]))
            force.assert_not_called()  # The supervisor must survive controller cancellation.
            deferred = panel._dispatch_pending(project, record)
            assert deferred["forced"] is False, "controller claimed supervisor force evidence"
            assert not panel._dispatch_call_finished(project, shared, room.participant(state, member))
            if scenario == "normal-unobserved":
                assert deferred["descendants"] is None, "failed observation was trusted as empty"
            if scenario in {"normal-provider-alive", "normal-unobserved"}:
                assert pending.wait.call_args_list[0].kwargs == {"timeout": 1}
                assert pending.wait.call_count == 2
            if scenario in provider_scenarios:
                assert deferred["descendants"] == [provider]
            # A stale or premature final handoff cannot release an active child's scope.
            if scenario not in provider_scenarios:
                room.send(project, shared, member, room_owner, "Call exit=143; parent acceptance pending.",
                          kind="handoff", message_id="result-" + member)
            assert any(o["participant"] == member for o in panel.scope_overlaps(
                project, shared, room.status(project, shared), room_owner, ["value.txt"]))
        panel.events(project, "transition", "--attempt", pending_attempt[0], "--state", "cancelled",
                     "--reason-code", "fixture_complete")
    # Negative lifetime fixtures intentionally reserve scopes. Finish their membership
    # so subsequent layout fixtures measure presentation rather than this test's pending calls.
    for member in room.status(project, shared)["participants"]:
        if member["participant"] not in original_members and member["joined"]:
            room.append(project, shared, {"kind": "leave", "participant": member["participant"]}, "Dispatch fixture complete")
    print("dispatch exit evidence: prelaunch refusal, durable receipts, missing evidence, subreaper orphan and unsupported pending passed")

# OPERATOR_SECTION_END dispatch_exit
PY
