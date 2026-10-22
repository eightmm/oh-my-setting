"""One section manifest and fixture execution path for every operator invocation."""
from collections import Counter
import ast
import os
from pathlib import Path
import re
import subprocess
import sys
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SOURCE = ROOT / "tests/operator-tools-smoke.sh"
# The integration chains retain order within their own files. Everything else
# starts with a fresh process, directory and explicitly declared fixture.
# Measured 2026-10-22/23; buckets follow measured section cost.
SECTIONS = (
    ("core/dashboard", 0, "dashboard.sh"),
    ("core/visual-and-controls", 1, "visual_and_controls.py"),
    ("core/room-protocol", 0, "room_protocol.py"),
    ("core/room-and-board-rendering", 1, "room_and_board_rendering.py"),
    ("core/native-integration", 2, "native_integration.py"),
    ("shared-cache", 2, "shared-cache.sh"),
    ("native-pty", 1, "native_pty.py"),
    ("dispatch-exit", 3, None),
    ("worker-leases", 1, "worker_leases.py"),
    ("dispatch-results", 2, "dispatch_results.py"),
    ("salvage", 2, "salvage.py"),
    ("scratch-state", 2, "scratch_state.py"),
    ("auto-review", 3, "auto_review.py"),
    ("room-rollover", 3, "room_rollover.py"),
)


def shard(spec):
    if not re.fullmatch(r"[1-9][0-9]*/[1-9][0-9]*", spec):
        raise ValueError("shard must be I/N with positive decimal integers")
    index, total = map(int, spec.split("/"))
    if not 1 <= index <= total <= 4:
        raise ValueError("shard index must be in 1..N and N in 1..4")
    return index, total


def requested_sections(spec):
    names = spec.split(",")
    valid = [name for name, _, _ in SECTIONS]
    if not names or any(name not in valid for name in names):
        raise ValueError("unknown section(s); valid sections: " + ", ".join(valid))
    if len(names) != len(set(names)):
        raise ValueError("duplicate section name")
    return names


def selected(spec):
    index, total = shard(spec)
    return [name for name, bucket, _ in SECTIONS if bucket % total == index - 1]


def body(name):
    filename = next(filename for section, _, filename in SECTIONS if section == name)
    if filename:
        return (HERE / filename).read_text(encoding="utf-8"), str(HERE / filename)
    source = SOURCE.read_text(encoding="utf-8")
    return (source.split("# OPERATOR_SECTION_BEGIN dispatch_exit\n", 1)[1].split(
        "# OPERATOR_SECTION_END dispatch_exit\n", 1)[0], str(SOURCE))


def self_check(total):
    shard("1/%s" % total)
    names = [name for name, _, _ in SECTIONS]
    if len(names) != len(set(names)):
        raise ValueError("duplicate operator section")
    files = [filename for _, _, filename in SECTIONS if filename]
    if len(files) != len(set(files)):
        raise ValueError("duplicate operator section file")
    expected = set(files) | {"runner.py", "fixtures.py", "helpers.py", "provider_fixture.py", "pty_fixture.py"}
    actual = {path.name for path in HERE.iterdir() if path.suffix in {".py", ".sh"}}
    if actual != expected:
        raise ValueError("unassigned operator module(s): " + repr(actual ^ expected))
    source = SOURCE.read_text(encoding="utf-8")
    for edge in ("BEGIN", "END"):
        if re.findall(r"^# OPERATOR_SECTION_" + edge + r" ([a-z_]+)$", source, re.M) != ["dispatch_exit"]:
            raise ValueError("dispatch-exit must have exactly one body")
    for name, _, filename in SECTIONS:
        if not filename or filename.endswith(".py"):
            ast.parse(body(name)[0], filename or str(SOURCE))
    for filename in expected - set(files):
        ast.parse((HERE / filename).read_text(encoding="utf-8"), filename)
    assignments = [name for index in range(1, total + 1) for name in selected("%s/%s" % (index, total))]
    if Counter(assignments) != Counter(names):
        raise ValueError("operator sections are unassigned or duplicated")
    if selected("1/1") != names or requested_sections(",".join(names)) != names:
        raise ValueError("serial and focused section manifests differ")
    execution_contract(total)
    return [(name, bucket % total + 1) for name, bucket, _ in SECTIONS]


def execute(name, temporary):
    os.chdir(temporary)
    namespace = {"root": ROOT, "temporary": temporary, "section": name}
    fixture = HERE / "fixtures.py"
    exec(compile(fixture.read_text(encoding="utf-8"), str(fixture), "exec"), namespace)
    namespace["_operator_block"] = lambda phase: print("operator-integration: " + phase, flush=True)
    code, filename = body(name)
    if name == "dispatch-exit":
        code += "\nexercise_dispatch_exit_evidence(root, temporary, project, shared, room_owner, room_env)\n"
    elif name == "worker-leases":
        code += '\nwith patch.object(panel, "provider_catalog", side_effect=fixture_provider_catalog):\n    exercise_observed_worker_leases()\n'
    if name == "auto-review":
        with namespace["patch"].object(namespace["panel"], "_settle_dispatch", return_value=None):
            exec(compile(code, filename, "exec"), namespace)
    else:
        exec(compile(code, filename, "exec"), namespace)


def run_sections(names, temporary):
    partial, failure = False, 0
    for name in names:
        socket_denied = os.environ.get("OMS_OPERATOR_SOCKET_DENIED") == "1"
        if socket_denied and name in {"core/room-protocol", "core/native-integration", "native-pty", "room-rollover"}:
            partial = True
        if socket_denied and name == "native-pty":
            print("operator-native-pty: skipped (sandbox denies unix sockets)", flush=True)
            partial = True
            continue
        directory = temporary / name.replace("/", "-")
        directory.mkdir()
        started = time.monotonic()
        filename = next(filename for section, _, filename in SECTIONS if section == name)
        if filename and filename.endswith(".sh"):
            command = ["bash", "-euo", "pipefail", "-c",
                       'ROOT="$1"; TMP="$2"; fail() { echo "FAIL: $*" >&2; exit 1; }; '
                       'operator_core_block() { echo "operator-integration: $1"; }; . "$3"',
                       "operator-section", str(ROOT), str(directory), str(HERE / filename)]
        else:
            command = [sys.executable, str(HERE / "runner.py"), "--execute", str(directory), name]
        print("operator-section: " + name + " started", flush=True)
        result = subprocess.run(command, cwd=directory)
        elapsed = int(time.monotonic() - started)
        timing = os.environ.get("OMS_OPERATOR_TIMINGS")
        if timing:
            with open(timing, "a", encoding="utf-8") as output:
                output.write("%s\t%s\n" % (elapsed, name))
        if result.returncode:
            failure = failure or result.returncode
            print("operator-section: %s FAILED (exit %s)" % (name, result.returncode), flush=True)
        else:
            print("operator-section: %s ok (%ds)" % (name, elapsed), flush=True)
    return failure or (77 if partial else 0)


def execution_contract(total):
    # Extend --self-check at the new process/fixture boundary. Compare the
    # actual launch commands of all modes, including after a section fails.
    from contextlib import redirect_stdout
    import io
    import tempfile
    from unittest.mock import patch

    names = [name for name, _, _ in SECTIONS]
    manifests = []
    with tempfile.TemporaryDirectory(prefix="oms-operator-contract-") as directory, \
            patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
        for mode in ("serial", "sharded", "focused", "failure"):
            observed = {}
            parent = Path(directory) / mode
            parent.mkdir()

            def launch(command, cwd):
                name = next(name for name in names if name.replace("/", "-") == cwd.name)
                assert name not in observed, (mode, name)
                observed[name] = [part.replace(str(cwd), "{fixture}") for part in command]
                return subprocess.CompletedProcess(command, int(mode == "failure" and name == names[0]))

            groups = ([selected("%s/%s" % (i, total)) for i in range(1, total + 1)]
                      if mode == "sharded" else [[name] for name in names] if mode == "focused" else [names])
            with patch.object(subprocess, "run", side_effect=launch):
                results = [run_sections(group, parent) for group in groups]
            assert set(observed) == set(names), (mode, observed)
            assert results == ([1] if mode == "failure" else [0] * len(groups)), (mode, results)
            manifests.append(observed)
    assert all(manifest == manifests[0] for manifest in manifests), "fixture launches depend on selection mode"


def main(args):
    mode = args[0]
    if mode in {"--list", "--self-check"} and len(args) == 2:
        total = int(args[1])
        for name, index in self_check(total):
            print("%s/%s\t%s" % (index, total, name))
    elif mode == "--list-sections" and len(args) == 1:
        self_check(4)
        print("\n".join(name for name, _, _ in SECTIONS))
    elif mode == "--execute" and len(args) == 3:
        name = requested_sections(args[2])[0]
        execute(name, Path(args[1]))
    elif mode in {"--run-shard", "--run-sections"} and len(args) == 3:
        self_check(shard(args[2])[1] if mode == "--run-shard" else 4)
        names = selected(args[2]) if mode == "--run-shard" else requested_sections(args[2])
        return run_sections(names, Path(args[1]))
    else:
        raise ValueError("invalid operator runner arguments")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, StopIteration) as error:
        if sys.argv[1:2] == ["--execute"]:
            raise
        print("error: " + str(error), file=sys.stderr)
        sys.exit(2)
