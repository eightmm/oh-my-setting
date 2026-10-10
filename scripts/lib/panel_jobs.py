"""Private argv jobs with one Linux subreaper per generation, without a service.

Only the supervisor signals its own descendants, using pidfds after /proc
identity checks. The leader stays unreaped until the generation completes, so
its PID/group cannot be reused while children survive. Missing supervisors are
unknown, never inferred exits. Only the latest generation's bounded log is kept.
"""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time

import panel_terminals as terminals

LOG_LIMIT = 1024 * 1024
GRACE_SECONDS = 2.0


def supported():
    terminals.supported()
    if sys.platform != "linux" or not Path("/proc/self/stat").exists():
        raise ValueError("registered jobs require Linux subreaper ownership")
    if not hasattr(os, "pidfd_open") or not hasattr(signal, "pidfd_send_signal"):
        raise ValueError("job control requires Linux pidfd support")
    fd = os.pidfd_open(os.getpid())
    os.close(fd)


def _recipe(store):
    recipe = store.read("recipe")
    argv = recipe.get("argv")
    if (recipe.get("repo") != str(store.repo) or recipe.get("uid") != os.getuid()
            or not isinstance(argv, list) or not argv or len(argv) > 256
            or any(not isinstance(arg, str) or "\0" in arg for arg in argv)
            or not argv[0] or not isinstance(recipe.get("cwd"), str)):
        raise ValueError("invalid private job recipe")
    return recipe


def _record(store, ident):
    row = terminals.validate_record(store, ident, "job")
    if (type(row.get("generation")) is not int or row["generation"] < 1
            or row.get("state") not in {"starting", "running", "stopping", "exited", "unknown"}):
        raise ValueError("invalid generation record")
    recipe = _recipe(store)
    if row.get("recipe_sha256") != hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest():
        raise ValueError("job recipe changed")
    fd = store.file("output")
    info = os.fstat(fd)
    os.close(fd)
    if row.get("output_identity") != [info.st_dev, info.st_ino]:
        raise ValueError("job log replaced")
    return row


def _supervisor_matches(row):
    if not terminals.matches(row.get("supervisor_pid"), row.get("supervisor_identity")):
        return False
    try:
        argv = Path("/proc/%s/cmdline" % row["supervisor_pid"]).read_bytes().split(b"\0")
        expected = [str(Path(__file__).resolve()), "_supervise", row["repo"], row["id"], str(row["generation"])]
        return [item.decode() for item in argv[1:6]] == expected
    except (OSError, UnicodeError):
        return False


def _leader_matches(row):
    leader = terminals.process(row.get("leader_pid"))
    return bool(leader and leader["identity"] == row.get("leader_identity")
                and leader["parent"] == row.get("supervisor_pid")
                and leader["group"] == row.get("leader_pid"))


def _state(row):
    if row["state"] == "exited" and row.get("completion") == "reaped-all-children":
        return "exited"
    if _supervisor_matches(row):
        if row["state"] == "starting" or _leader_matches(row):
            return row["state"]
    return "unknown"


def _public(row):
    state = _state(row)
    return {"id": row["id"], "label": terminals.label(row["label"]), "kind": "job",
            "state": state, "generation": row["generation"],
            "exit_code": row.get("exit_code") if state == "exited" else None,
            "capabilities": {"logs": True, "interrupt": state == "running" and not row.get("request"),
                             "stop": state == "running" and not row.get("request"),
                             "restart": state == "exited", "forget": state == "exited"},
            "log_truncated": bool(row.get("log_truncated")), "log_limit": LOG_LIMIT,
            "log_retention": "latest generation only"}


def _start(store, row, prepare=None, rollback=None):
    read_gate, write_gate = os.pipe()
    child = None
    releasing = False
    try:
        command = [sys.executable, str(Path(__file__).resolve()), "_supervise", str(store.repo),
                   row["id"], str(row["generation"]), str(read_gate), str(store.state_home)]
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, start_new_session=True,
                                 pass_fds=(read_gate,), env=terminals.ordinary_environment())
        identity = terminals.process(child.pid)
        if not identity:
            raise ValueError("supervisor identity unavailable")
        row.update(supervisor_pid=child.pid, supervisor_identity=identity["identity"])
        if prepare is not None:
            prepare()
        store.write(row)
        releasing = True
        os.write(write_gate, b"1")
    except BaseException:
        # Until the approval write begins, even a spawned supervisor cannot
        # execute the recipe; closing the pipe below aborts that generation.
        if not releasing and rollback is not None:
            rollback()
        elif child is not None:
            row["state"] = "unknown"
            store.write(row)
        raise
    finally:
        os.close(read_gate)
        os.close(write_gate)


def _observe(repo, ident, generation=None):
    """One public snapshot read under the supervisor's record lock.

    The supervisor holds this lock from reaping the leader until it publishes
    completion; an unlocked read in that window sees a reaped leader beside a
    still-running record and would report a false unknown.
    """
    with terminals.Store(repo, "jobs", ident) as store:
        with store.lock():
            row = _record(store, ident)
            if generation is not None and row["generation"] != generation:
                raise ValueError("job generation changed")
            return _public(row)


def _wait_started(repo, ident, generation):
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        snapshot = _observe(repo, ident, generation)
        if snapshot["state"] != "starting":
            return snapshot
        time.sleep(.03)
    raise ValueError("job launch not confirmed; inspect its state before retrying")


def run_job(repo, session, argv, *, label="Job", cwd=None):
    terminals.parent_only()
    supported()
    repo = terminals.canonical(repo)
    binding = terminals.session_proof(repo, session)
    cwd = terminals.canonical(cwd if cwd is not None else repo)
    if (not isinstance(argv, (list, tuple)) or not argv or len(argv) > 256
            or any(not isinstance(arg, str) or "\0" in arg for arg in argv) or not argv[0]):
        raise ValueError("an explicit argv recipe is required")
    recipe = {"repo": str(repo), "uid": os.getuid(), "cwd": str(cwd), "argv": list(argv)}
    if len(json.dumps(recipe).encode()) > terminals.MAX_RECORD:
        raise ValueError("job recipe is too large")
    ident = terminals.new_store(repo, "jobs", "job")
    with terminals.Store(repo, "jobs", ident) as store:
        with store.lock():
            store.write(recipe, "recipe")
            fd = store.file("output", os.O_WRONLY | os.O_EXCL, create=True)
            info = os.fstat(fd)
            os.close(fd)
            row = dict(binding, schema=1, id=ident, repo=str(repo), uid=os.getuid(), kind="job",
                       state_home=str(store.state_home),
                       label=terminals.label(label), generation=1, state="starting", log_truncated=False,
                       recipe_sha256=hashlib.sha256(json.dumps(recipe, sort_keys=True).encode()).hexdigest(),
                       output_identity=[info.st_dev, info.st_ino])
            store.write(row)
            # Revalidate after recording and immediately before a launch can occur.
            if terminals.session_proof(repo, session) != binding:
                raise ValueError("panel session changed")
            _start(store, row)
    return _wait_started(repo, ident, 1)


def list_jobs(repo):
    terminals.parent_only()
    supported()
    rows = []
    for ident in terminals.identifiers(repo, "jobs"):
        rows.append(_observe(repo, ident))
    return rows


def job_logs(repo, ident, generation, *, maximum=terminals.MAX_READ):
    terminals.parent_only()
    with terminals.Store(repo, "jobs", ident) as store:
        with store.lock():
            row = _record(store, ident)
            if row["generation"] != generation:
                raise ValueError("stale generation")
            fd = store.file("output")
            try:
                size = os.fstat(fd).st_size
                raw = os.read(fd, LOG_LIMIT)
            finally:
                os.close(fd)
            result = terminals.preview(raw, maximum)
            result.update(id=ident, kind="job", generation=generation,
                          truncated=result["truncated"] or bool(row.get("log_truncated")) or size > len(raw),
                          retention="latest generation only; first 1048576 bytes retained")
            return result


def forget_job(repo, ident, generation):
    if type(generation) is not int or generation < 1:
        raise ValueError("exact job generation required")
    def proof(store, row):
        current = _record(store, ident)
        if current["generation"] != generation or _state(current) != "exited":
            raise ValueError("only a confirmed completed generation can be forgotten")
    return terminals.forget_record(repo, "jobs", ident, proof, ("record", "recipe", "output"))


def _request(repo, ident, generation, action):
    terminals.parent_only()
    supported()
    with terminals.Store(repo, "jobs", ident) as store:
        with store.lock():
            row = _record(store, ident)
            if row["generation"] != generation or _state(row) != "running":
                raise ValueError("job ownership/generation is stale or mutation is unavailable")
            # The caller never signals a PID. The bound supervisor consumes this
            # request under the same lock and independently proves descendants.
            if row.get("request"):
                raise ValueError("job already has a pending control request")
            row["request"] = action
            store.write(row)
            return _public(row)


def interrupt_job(repo, ident, generation):
    return _request(repo, ident, generation, "interrupt")


def stop_job(repo, ident, generation):
    _request(repo, ident, generation, "stop")
    deadline = time.monotonic() + GRACE_SECONDS + 3
    while True:
        snapshot = _observe(repo, ident, generation)
        if snapshot["state"] in {"exited", "unknown"} or time.monotonic() >= deadline:
            return snapshot
        time.sleep(.05)


def restart_job(repo, session, ident, generation):
    terminals.parent_only()
    supported()
    binding = terminals.session_proof(repo, session)
    with terminals.Store(repo, "jobs", ident) as store:
        with store.lock():
            old = _record(store, ident)
            if old["generation"] != generation or _state(old) != "exited":
                raise ValueError("restart requires the confirmed exited generation")
            recipe = _recipe(store)
            terminals.canonical(recipe["cwd"])
            fd = store.file("output")
            try:
                previous_log = os.read(fd, LOG_LIMIT + 1)
            finally:
                os.close(fd)
            if len(previous_log) > LOG_LIMIT:
                raise ValueError("previous log exceeds the retention contract")
            row = dict(binding, schema=1, id=ident, repo=str(store.repo), uid=os.getuid(), kind="job",
                       state_home=str(store.state_home),
                       label=old["label"], generation=generation + 1, state="starting", log_truncated=False,
                       recipe_sha256=old["recipe_sha256"], output_identity=old["output_identity"])
            if terminals.session_proof(repo, session) != binding:
                raise ValueError("panel session changed")
            def clear_log():
                fd = store.file("output", os.O_WRONLY)
                try:
                    os.ftruncate(fd, 0)
                finally:
                    os.close(fd)
            def restore():
                fd = store.file("output", os.O_WRONLY)
                try:
                    os.ftruncate(fd, 0)
                    with os.fdopen(fd, "wb") as stream:
                        fd = None
                        stream.write(previous_log)
                finally:
                    if fd is not None:
                        os.close(fd)
                store.write(old)
            # The record lock excludes another restart. The supervisor waits
            # behind its pipe until the new record and log are ready.
            _start(store, row, prepare=clear_log, rollback=restore)
    return _wait_started(repo, ident, generation + 1)


def _descendants():
    """Walk actual children, including children adopted after setsid/double fork."""
    result = []
    pending = [os.getpid()]
    visited = set(pending)
    while pending:
        pid = pending.pop()
        try:
            # A child can fork from any thread, not only its main thread.
            tasks = list(Path("/proc/%s/task" % pid).iterdir())
        except FileNotFoundError:
            continue
        for task in tasks:
            try:
                children = (task / "children").read_text().split()
            except FileNotFoundError:
                continue
            for value in children:
                child = int(value)
                if child in visited:
                    continue
                visited.add(child)
                info = terminals.process(child)
                if info is not None:
                    result.append(info)
                    pending.append(child)
    return result


def _signal_owned(children, sig):
    for child in reversed(children):
        if child["state"] in {"Z", "X"}:
            continue
        try:
            fd = os.pidfd_open(child["pid"])
            try:
                current = terminals.process(child["pid"])
                if not current or current["identity"] != child["identity"]:
                    raise ValueError("descendant identity changed")
                signal.pidfd_send_signal(fd, sig)
            finally:
                os.close(fd)
        except ProcessLookupError:
            pass


def _supervise(repo, ident, generation, gate, base):
    try:
        approved = os.read(gate, 1) == b"1"
    finally:
        os.close(gate)
    if not approved:
        return
    supported()
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), "subreaper unavailable")
    with terminals.Store(repo, "jobs", ident, base=base) as store:
        with store.lock():
            row = _record(store, ident)
            if (row["generation"] != generation or row["state"] != "starting"
                    or row.get("supervisor_pid") != os.getpid() or not _supervisor_matches(row)):
                raise ValueError("supervisor claim mismatch")
            recipe = _recipe(store)
            if terminals.session_proof(repo, row["session"]) != {
                    key: row[key] for key in ("session", "server_pid", "server_identity", "socket")}:
                raise ValueError("launch session changed")
            log = store.file("output", os.O_WRONLY | os.O_APPEND)
            try:
                child = subprocess.Popen(recipe["argv"], cwd=recipe["cwd"],
                                         env=terminals.ordinary_environment(), start_new_session=True,
                                         stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
            except OSError:
                os.close(log)
                row.update(state="exited", completion="reaped-all-children", exit_code=127)
                store.write(row)
                return
            identity = terminals.process(child.pid)
            row.update(state="running", leader_pid=child.pid, leader_identity=identity["identity"])
            store.write(row)
        selector = selectors.DefaultSelector()
        os.set_blocking(child.stdout.fileno(), False)
        selector.register(child.stdout, selectors.EVENT_READ)
        retained = 0
        truncated = False
        stopping = None
        try:
            while True:
                # Always drain beyond retention; a verbose child must never block
                # merely because the UI log budget has been exhausted.
                for key, unused in selector.select(.05):
                    data = os.read(key.fd, 65536)
                    if not data:
                        selector.unregister(key.fileobj)
                    else:
                        keep = data[:max(0, LOG_LIMIT - retained)]
                        if keep:
                            os.write(log, keep)
                            retained += len(keep)
                        truncated = truncated or len(keep) < len(data)
                with store.lock():
                    row = _record(store, ident)
                    if (row["generation"] != generation or not _supervisor_matches(row)
                            or not _leader_matches(row)):
                        raise ValueError("supervisor ownership changed")
                    children = _descendants()
                    request = row.pop("request", None)
                    if request == "interrupt":
                        _signal_owned(children, signal.SIGINT)
                    elif request == "stop":
                        stopping = time.monotonic()
                        row["state"] = "stopping"
                        _signal_owned(children, signal.SIGTERM)
                    elif request is not None:
                        raise ValueError("invalid control request")
                    if stopping is not None and time.monotonic() - stopping >= GRACE_SECONDS:
                        _signal_owned(children, signal.SIGKILL)
                    row["log_truncated"] = truncated
                    # Reap adopted zombies promptly while keeping the leader
                    # pinned until all descendants and the output pipe finish.
                    for item in children:
                        if item["pid"] != child.pid and item["state"] in {"Z", "X"}:
                            try:
                                os.waitpid(item["pid"], os.WNOHANG)
                            except ChildProcessError:
                                pass
                    alive = any(item["state"] not in {"Z", "X"} for item in children)
                    if not alive and not selector.get_map():
                        # Reparenting can race a tree scan. The kernel's direct
                        # child list must contain only the pinned zombie leader
                        # before it is safe to reap it and certify completion.
                        direct = Path("/proc/self/task/%s/children" % os.getpid()).read_text().split()
                        if direct == [str(child.pid)]:
                            pid, status = os.waitpid(child.pid, 0)
                            try:
                                os.waitpid(-1, os.WNOHANG)
                            except ChildProcessError:
                                exit_code = os.waitstatus_to_exitcode(status)
                                child.returncode = exit_code
                                row.update(state="exited", completion="reaped-all-children", exit_code=exit_code)
                                store.write(row)
                                return
                            raise ValueError("unexpected child after completion proof")
                    if request or row["log_truncated"] != bool(store.read().get("log_truncated")):
                        store.write(row)
        finally:
            selector.close()
            child.stdout.close()
            os.close(log)


def cli_main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("run", "list", "logs", "interrupt", "stop", "restart", "forget"))
    parser.add_argument("id", nargs="?")
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--session")
    parser.add_argument("--cwd", type=Path)
    parser.add_argument("--label", default="Job")
    parser.add_argument("--generation", type=int)
    parser.add_argument("--maximum", type=int, default=terminals.MAX_READ)
    parser.add_argument("--json", action="store_true")
    values = list(sys.argv[1:] if argv is None else argv)
    command = []
    if "--" in values:
        at = values.index("--")
        command, values = values[at + 1:], values[:at]
    args = parser.parse_args(values)
    try:
        if args.action == "run":
            result = run_job(args.repo, args.session, command, label=args.label, cwd=args.cwd)
        elif args.action == "list":
            result = list_jobs(args.repo)
        elif args.action == "logs":
            result = job_logs(args.repo, args.id, args.generation, maximum=args.maximum)
        elif args.action == "restart":
            result = restart_job(args.repo, args.session, args.id, args.generation)
        elif args.action == "forget":
            result = forget_job(args.repo, args.id, args.generation)
        else:
            action = interrupt_job if args.action == "interrupt" else stop_job
            result = action(args.repo, args.id, args.generation)
        print(json.dumps(result, ensure_ascii=True))
        return 0
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        print("managed job action refused: ownership or generation unavailable", file=sys.stderr)
        return 1


if __name__ == "__main__":
    if len(sys.argv) == 7 and sys.argv[1] == "_supervise":
        try:
            _supervise(sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5]), sys.argv[6])
        except (OSError, ValueError, KeyError, subprocess.SubprocessError):
            sys.exit(1)
    else:
        sys.exit(cli_main())
