"""One section manifest and fixture execution path for every operator invocation.

--list reports whole sections and dispatch case units, including the unmarked
integration remainder. Focused sections and --case keep their existing meaning;
shards group their dispatch cases in source order under one private fixture.
"""
from collections import Counter
from functools import lru_cache
import ast
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
SOURCE = ROOT / "tests/operator-tools-smoke.sh"
# The integration chains retain order within their own files. Everything else
# starts with a fresh process, directory and explicitly declared fixture.
# Costs are rounded medians of the latest four successful per-section timings
# in land gate logs available 2026-11-08 (one host; source and contention vary).
# dispatch-results was 906/1211/1117/1340s, rather than the old 287s weight.
# Sharded dispatch-results timing rows now cover subsets; refresh its total
# from the sum of all shard timings, not a wall-time or subset row.
# core/room-protocol stays one section: its later phases reuse the state its
# earlier phases build (room_env, shared, main_attempts, plan_cli).
SECTIONS = (
    ("core/dashboard", 20, "dashboard.sh"),
    ("core/visual-and-controls", 33, "visual_and_controls.py"),
    ("core/room-protocol", 879, "room_protocol.py"),
    ("core/room-and-board-rendering", 19, "room_and_board_rendering.py"),
    ("core/native-integration", 298, "native_integration.py"),
    ("shared-cache", 117, "shared-cache.sh"),
    ("native-pty", 129, "native_pty.py"),
    ("dispatch-exit", 266, None),
    ("worker-leases", 587, "worker_leases.py"),
    ("dispatch-results", 1164, "dispatch_results.py"),
    ("salvage", 2, "salvage.py"),
    ("scratch-state", 14, "scratch_state.py"),
    ("auto-review", 232, "auto_review.py"),
    ("room-rollover", 338, "room_rollover.py"),
)

# The unmarked integration chain shares mutable state and stays one unit.
# Marked cases use the same definitions/literals as --case, once per shard,
# without replaying that chain. A single successful module-line profile timed
# all 25 cases plus the remainder in 1480s (1484s wall, traced/contended).
# These proportions scale to the 1164s gate median; subsecond cases round to
# 1s. They are placement estimates, not measured speedups. Three setup probes
# took 1.326..1.392s for fixtures and 0.002..0.003s for shared declarations per
# dispatch process; allow another 2s per shard beyond the unit-weight sums.
# Four-way estimated sums with that allowance: 1030/1030/1029/1029s.
DISPATCH_REMAINDER = "integration remainder"
DISPATCH_SECONDS = {
    "continuation round delta": 1,
    "continued acceptance regression": 1,
    "results task index": 1,
    "tier routing": 1,
    "settled overlap warning": 1,
    "continuation admission hint": 1,
    "continuation route provenance": 1,
    "continuation outbound temporary file": 1,
    "verify selector validation": 1,
    "sole writer review refusal": 1,
    "pinned judge result label": 3,
    "explicit judge families": 1,
    "family exclusion quota": 1,
    "council resolved catalog model": 1,
    "council help families": 1,
    "council extra seats": 4,
    "council prompt defaults": 7,
    "council lens seats": 33,
    "finalized survey next-ready projection": 1,
    "finalized survey plan hint": 1,
    "reviewed-result acceptance composition": 223,
    "patch-scope finalize regression": 6,
    "batch-landed finalize proof": 17,
    "Provider account routing": 1,
    "Provider account execution": 481,
    "Provider account retry evidence": 1,
    DISPATCH_REMAINDER: 401,
}


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
    names = {name for name, _, bucket in assignments(total) if bucket == index}
    return [name for name, _, _ in SECTIONS if name in names]


def assignments(total):
    shard("1/%s" % total)
    units = [(name, None, seconds) for name, seconds, _ in SECTIONS if name != "dispatch-results"]
    units += [("dispatch-results", case, seconds) for case, seconds in DISPATCH_SECONDS.items()]
    loads, placed = [0] * total, {}
    # Longest-first placement keeps the indivisible stateful chains together
    # while filling the remaining capacity with independent dispatch cases.
    for name, case, seconds in sorted(units, key=lambda unit: -unit[2]):
        bucket = min(range(total), key=lambda index: loads[index])
        loads[bucket] += seconds
        placed[name, case] = bucket + 1
    return [(name, case, placed[name, case]) for name, case, _ in units]


def body(name):
    filename = next(filename for section, _, filename in SECTIONS if section == name)
    if filename:
        return (HERE / filename).read_text(encoding="utf-8"), str(HERE / filename)
    source = SOURCE.read_text(encoding="utf-8")
    head, rest = source.split("# OPERATOR_SECTION_BEGIN dispatch_exit\n", 1)
    # Blank lines up to and including the marker keep traceback numbers equal to SOURCE's lines.
    padding = "\n" * (head.count("\n") + 1)
    return padding + rest.split("# OPERATOR_SECTION_END dispatch_exit\n", 1)[0], str(SOURCE)


@lru_cache(maxsize=1)
def case_markers(code):
    """Top-level `# BEGIN X` / `# END X` pairs as {case: (begin_line, end_line)}, 1-based."""
    found, open_case = {}, None
    for number, line in enumerate(code.splitlines(), 1):
        match = re.match(r"# (BEGIN|END) (.+)$", line)
        if not match:
            continue
        kind, text = match.groups()
        case = re.split(r" \(|\.", text, 1)[0].strip()
        if kind == "BEGIN":
            if open_case:
                raise ValueError("case %r begins inside case %r" % (case, open_case[0]))
            if case in found:
                raise ValueError("duplicate case %r" % case)
            open_case = (case, number)
        elif not open_case or open_case[0] != case:
            raise ValueError("END %r has no matching BEGIN" % case)
        else:
            found[case] = (open_case[1], number)
            open_case = None
    if open_case:
        raise ValueError("BEGIN %r has no END" % open_case[0])
    return found


@lru_cache(maxsize=1)
def parsed(code, filename):
    # Callers only inspect these trees. Source bytes keep edits at one path distinct.
    return ast.parse(code, filename)


def case_body(name, case):
    if not case:
        raise ValueError("--case requires a non-empty CASE")
    filename = next(filename for section, _, filename in SECTIONS if section == name)
    if filename and filename.endswith(".sh"):
        raise ValueError("--case supports Python sections only; run --section %s without --case" % name)
    code, filename = body(name)
    found = case_markers(code)
    if case not in found:
        raise ValueError("unknown case %r in section %s; valid cases: %s" % (case, name, ", ".join(found) or "none"))
    begin, end = found[case]
    keep = set(range(begin + 1, end))
    # Definitions outside every case stay so a block runs without its predecessors' setup.
    # An assignment stays only if every name it reads is already available.
    always, assigns = [], []
    for node in parsed(code, filename).body:
        first = min([node.lineno] + [d.lineno for d in getattr(node, "decorator_list", [])])
        if any(first <= close and node.end_lineno >= open_ for open_, close in found.values()):
            continue
        if isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            always.append((first, node))
        elif isinstance(node, (ast.Assign, ast.AnnAssign)) and node.value is not None \
                and not any(isinstance(n, (ast.Call, ast.Await, ast.Yield, ast.YieldFrom)) for n in ast.walk(node.value)) \
                and not any(isinstance(n, ast.Subscript) for n in ast.walk(node)):  # containers may be filled by skipped code
            assigns.append((first, node))
    available = set(dir(__import__("builtins")))  # fixture values are runtime state, not literals
    for _, node in always:
        available |= {n.asname or n.name.split(".")[0] for n in getattr(node, "names", [])} | {getattr(node, "name", "")}
    changed = True
    while changed:
        changed = False
        for item in list(assigns):
            node = item[1]
            reads = {n.id for n in ast.walk(node) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load)}
            if reads <= available:
                targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                available |= {n.id for t in targets for n in ast.walk(t) if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Store)}
                always.append(item)
                assigns.remove(item)
                changed = True
    for first, node in always:
        keep.update(range(first, node.end_lineno + 1))
    # Blank lines elsewhere keep traceback numbers equal to the file's lines.
    return "\n".join(line if number in keep else "" for number, line in enumerate(code.splitlines(), 1)) + "\n", filename


def dispatch_body(spec):
    index, total = shard(spec)
    cases = [case for name, case, bucket in assignments(total)
             if name == "dispatch-results" and bucket == index]
    code, filename = body("dispatch-results")
    markers = case_markers(code)
    if set(DISPATCH_SECONDS) != set(markers) | {DISPATCH_REMAINDER}:
        raise ValueError("dispatch case costs differ from the case manifest")
    # Union the existing standalone extraction, retaining source order and line
    # numbers. One fixture per shard amortizes setup across its selected cases.
    extracted = [case_body("dispatch-results", case)[0].splitlines()
                 for case in cases if case != DISPATCH_REMAINDER]
    unmarked = set(range(1, len(code.splitlines()) + 1))
    for begin, end in markers.values():
        unmarked.difference_update(range(begin, end + 1))
    return "\n".join(line if any(part[number - 1] for part in extracted)
                     or DISPATCH_REMAINDER in cases and number in unmarked else ""
                     for number, line in enumerate(code.splitlines(), 1)) + "\n", filename


def module_names(code, filename, lines=None):
    """(bound, loaded) module names; loads come from module level and, transitively, from
    the top-level defs it names or that start inside `lines` (all of them when None)."""
    import symtable
    table = symtable.symtable(code, filename, "exec")
    bound = {s.get_name() for s in table.get_symbols() if s.is_assigned() or s.is_imported() or s.is_namespace()}

    def reads(scope):
        found, stack = set(), [scope]
        while stack:
            current = stack.pop()
            stack.extend(current.get_children())
            found |= {s.get_name() for s in current.get_symbols() if s.is_referenced()
                      and (s.is_global())}
        return found - bound

    defs = {}
    for child in table.get_children():
        defs.setdefault(child.get_name(), []).append(child)
    # symtable marks a module name referenced when only functions use it, so module-level
    # loads come from the AST outside function bodies (comprehension targets count as bound).
    tree = ast.parse(code, filename)
    # Only module-scope bindings (comprehension targets included) count; names bound after a
    # block or only in its functions' locals are a known limit of this ordering-free check.
    stored, walk = set(), list(tree.body)
    while walk:
        node = walk.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)):
            continue
        if isinstance(node, ast.Name) and not isinstance(node.ctx, ast.Load):
            stored.add(node.id)
        walk.extend(ast.iter_child_nodes(node))
    loaded, stack = set(), list(tree.body)
    while stack:
        node = stack.pop()
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.Lambda)):
            stack.extend(node.decorator_list if not isinstance(node, ast.Lambda) else [])
            stack.extend(node.args.defaults + [d for d in node.args.kw_defaults if d])
            continue
        if isinstance(node, ast.Name) and isinstance(node.ctx, ast.Load):
            loaded.add(node.id)
        stack.extend(ast.iter_child_nodes(node))
    loaded -= stored
    queue = [c for cs in defs.values() for c in cs if lines is None or c.get_lineno() in lines]
    seen = set()
    while True:
        for child in queue:
            loaded |= reads(child)
        queue = [c for name in loaded - seen for c in defs.get(name, [])]
        seen |= loaded
        if not queue:
            return bound, loaded - bound


@lru_cache(maxsize=4)
def fixture_names(code, filename):
    return module_names(code, filename)[0]


def provided_names():
    import builtins
    names = {"root", "temporary", "section", "_operator_block", "fixture_file"} | set(dir(builtins))
    for fixture in ("fixtures.py", "helpers.py", "provider_fixture.py", "pty_fixture.py"):
        names |= fixture_names((HERE / fixture).read_text(encoding="utf-8"), fixture)
    return names


def unresolved_case_names(name, case):
    code, filename = case_body(name, case)
    begin, end = case_markers(body(name)[0])[case]
    bound, loaded = module_names(code, filename, set(range(begin + 1, end)))
    provided = provided_names()
    return sorted(loaded - bound - provided)


def self_check(total):
    parsed.cache_clear()
    fixture_names.cache_clear()
    case_markers.cache_clear()
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
    for name, _, filename in SECTIONS:
        if not filename or filename.endswith(".py"):
            for case in case_markers(body(name)[0]):
                missing = unresolved_case_names(name, case)
                if missing:
                    raise ValueError("case %r in section %s cannot run alone; unresolved names: %s" % (case, name, ", ".join(missing)))
    tree = ast.parse(body("dispatch-exit")[0], str(SOURCE))
    defined = [node.lineno for node in tree.body if isinstance(node, ast.FunctionDef)
               and node.name == "exercise_dispatch_exit_evidence"]
    declared = [number for number, line in enumerate(source.splitlines(), 1)
                if line.startswith("def exercise_dispatch_exit_evidence(")]
    if defined != declared:
        raise ValueError("dispatch-exit traceback lines differ from the source file")
    for filename in expected - set(files):
        ast.parse((HERE / filename).read_text(encoding="utf-8"), filename)
    expected_units = [(name, None) for name in names if name != "dispatch-results"]
    expected_units += [("dispatch-results", case) for case in case_markers(body("dispatch-results")[0])]
    expected_units.append(("dispatch-results", DISPATCH_REMAINDER))
    assigned = Counter((name, case) for name, case, _ in assignments(total))
    if assigned != Counter(expected_units):
        delta = (assigned - Counter(expected_units)) + (Counter(expected_units) - assigned)
        raise ValueError("operator execution units are unassigned or duplicated (register dispatch-results "
                         "cases in DISPATCH_SECONDS): " + ", ".join(sorted("%s[%s]" % unit for unit in delta)))
    if selected("1/1") != names or requested_sections(",".join(names)) != names:
        raise ValueError("serial and focused section manifests differ")
    execution_contract(total)
    parsed.cache_clear()
    fixture_names.cache_clear()
    case_markers.cache_clear()
    # --list describes execution units; the remainder is not a standalone --case.
    return [(name + (" [" + case + "]" if case is not None else ""), bucket)
            for name, case, bucket in assignments(total)]


def execute(name, temporary, case=None, spec=None):
    # Refuse unsupported selectors before initializing section fixtures.
    if spec is not None:
        code, filename = dispatch_body(spec)
    else:
        code, filename = case_body(name, case) if case is not None else body(name)
    os.chdir(temporary)
    namespace = {"root": ROOT, "temporary": temporary, "section": name}
    fixture = HERE / "fixtures.py"
    exec(compile(fixture.read_text(encoding="utf-8"), str(fixture), "exec"), namespace)
    namespace["_operator_block"] = lambda phase: print("operator-integration: " + phase, flush=True)
    if name == "dispatch-exit" and case is None:
        code += "\nexercise_dispatch_exit_evidence(root, temporary, project, shared, room_owner, room_env)\n"
    elif name == "worker-leases" and case is None:
        code += '\nwith patch.object(panel, "provider_catalog", side_effect=fixture_provider_catalog):\n    exercise_observed_worker_leases()\n'
    if name == "auto-review":
        with namespace["patch"].object(namespace["panel"], "_settle_dispatch", return_value=None):
            exec(compile(code, filename, "exec"), namespace)
    else:
        exec(compile(code, filename, "exec"), namespace)


def run_dispatch_shards(directory):
    children = []
    previous = signal.getsignal(signal.SIGTERM)

    def interrupted(signum, frame):
        raise SystemExit(128 + signum)

    signal.signal(signal.SIGTERM, interrupted)
    try:
        for index in (1, 2):
            shard_dir = directory / str(index)
            shard_dir.mkdir()
            output = tempfile.TemporaryFile(mode="w+b")
            try:
                command = [sys.executable, str(HERE / "runner.py"), "--execute-shard",
                           str(shard_dir), "dispatch-results", "%s/2" % index]
                process = subprocess.Popen(command, cwd=shard_dir, stdout=output,
                                           stderr=subprocess.STDOUT, start_new_session=True)
            except BaseException:
                output.close()
                raise
            children.append((index, process, output))
        pending = list(children)
        failure = 0
        while pending:
            for item in pending[:]:
                index, process, output = item
                result = process.poll()
                if result is None:
                    continue
                output.seek(0)
                sys.stdout.flush()
                data = output.read().decode("utf-8", errors="replace")
                if data:
                    print(data, end="" if data.endswith("\n") else "\n", flush=True)
                print("operator-section: dispatch-results shard %s/2 %s" %
                      (index, "ok" if result == 0 else "FAILED (exit %s)" % result), flush=True)
                failure = failure or result
                pending.remove(item)
            if pending:
                time.sleep(0.1)
        return failure
    finally:
        for _, process, output in children:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
        for _, process, output in children:
            if process.poll() is None:
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    process.wait()
            output.close()
        signal.signal(signal.SIGTERM, previous)


def run_sections(names, temporary, case=None, spec=None):
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
            if case is not None:
                command += ["--case", case]
            elif name == "dispatch-results" and spec is not None and shard(spec)[1] > 1:
                command[2] = "--execute-shard"
                command.append(spec)
        print("operator-section: " + name + " started", flush=True)
        if name == "dispatch-results" and case is None and spec is None:
            returncode = run_dispatch_shards(directory)
        else:
            returncode = subprocess.run(command, cwd=directory).returncode
        elapsed = int(time.monotonic() - started)
        timing = os.environ.get("OMS_OPERATOR_TIMINGS")
        if timing:
            with open(timing, "a", encoding="utf-8") as output:
                output.write("%s\t%s\n" % (elapsed, name))
        if returncode:
            failure = failure or returncode
            print("operator-section: %s FAILED (exit %s)" % (name, returncode), flush=True)
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

    for filename in ("first.py", "second.py"):
        parsed("pass\n", filename)
        try:
            parsed("def invalid(\n", filename)
        except SyntaxError as error:
            assert error.filename == filename, error
        else:
            raise AssertionError("invalid source reused a cached tree")

    names = [name for name, _, _ in SECTIONS]
    manifests = []
    for exit_code in (0, 1, 77):
        with tempfile.TemporaryDirectory(prefix="oms-operator-contract-") as directory, \
            patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()) as captured:
            observed = []
            timing = Path(directory) / "timings"
            os.environ["OMS_OPERATOR_TIMINGS"] = str(timing)

            class Child:
                def __init__(self, command, cwd, **kwargs):
                    observed.append((command, cwd))
                    self.returncode = exit_code if command[-1] == "1/2" else 0
                    kwargs["stdout"].write((command[-1] + " output\n").encode())

                def poll(self):
                    assert len(observed) == 2, "both shards must start before waiting"
                    return self.returncode

            with patch.object(subprocess, "Popen", Child), \
                    patch.object(subprocess, "run", side_effect=AssertionError("bare section launched serially")):
                result = run_sections(["dispatch-results"], Path(directory))
            assert result == exit_code and [command[-1] for command, _ in observed] == ["1/2", "2/2"]
            assert all(command[2] == "--execute-shard" for command, _ in observed)
            assert len({cwd for _, cwd in observed}) == 2
            assert "1/2 output\n" in captured.getvalue() and "2/2 output\n" in captured.getvalue()
            rows = timing.read_text(encoding="utf-8").splitlines()
            assert len(rows) == 1 and re.fullmatch(r"[0-9]+\tdispatch-results", rows[0]), rows
    with tempfile.TemporaryDirectory(prefix="oms-operator-contract-") as directory, \
            patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
        fixtures = Path(directory) / "fixtures"
        fixtures.mkdir()
        for filename in ("fixtures.py", "helpers.py", "provider_fixture.py", "pty_fixture.py"):
            (fixtures / filename).write_text("before_edit = 1\n", encoding="utf-8")
        with patch.dict(globals(), HERE=fixtures):
            assert "before_edit" in provided_names() and "after_edit" not in provided_names()
            (fixtures / "helpers.py").write_text("after_edit = 1\n", encoding="utf-8")
            assert "after_edit" in provided_names(), "fixture cache reused source from before an edit"
        for mode in ("serial", "focused", "failure"):
            observed = {}
            parent = Path(directory) / mode
            parent.mkdir()

            def launch(command, cwd):
                name = next(name for name in names if name.replace("/", "-") == cwd.name)
                assert name not in observed, (mode, name)
                observed[name] = [part.replace(str(cwd), "{fixture}") for part in command]
                return subprocess.CompletedProcess(command, int(mode == "failure" and name == names[0]))

            groups = [[name] for name in names] if mode == "focused" else [names]
            def bare(directory):
                command = [sys.executable, str(HERE / "runner.py"), "--execute", str(directory), "dispatch-results"]
                return launch(command, directory).returncode

            with patch.object(subprocess, "run", side_effect=launch), patch.dict(globals(), run_dispatch_shards=bare):
                results = [run_sections(group, parent) for group in groups]
            assert set(observed) == set(names), (mode, observed)
            assert results == ([1] if mode == "failure" else [0] * len(groups)), (mode, results)
            manifests.append(observed)
    assert all(manifest == manifests[0] for manifest in manifests), "fixture launches depend on selection mode"
    with tempfile.TemporaryDirectory(prefix="oms-operator-contract-") as directory, \
            patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
        observed = []
        for index in range(1, total + 1):
            spec = "%s/%s" % (index, total)
            parent = Path(directory) / str(index)
            parent.mkdir()

            def launch(command, cwd):
                name = next(name for name in names if name.replace("/", "-") == cwd.name)
                normalized = [part.replace(str(cwd), "{fixture}") for part in command]
                expected = list(manifests[0][name])
                if name == "dispatch-results" and total > 1:
                    expected[2] = "--execute-shard"
                    expected.append(spec)
                assert normalized == expected, (spec, name, normalized)
                observed.append(name)
                return subprocess.CompletedProcess(command, int(name == "dispatch-results"))

            with patch.object(subprocess, "run", side_effect=launch):
                result = run_sections(selected(spec), parent, spec=spec)
            assert result == int("dispatch-results" in selected(spec)), (spec, result)
        assert Counter(observed) == Counter(name for name, _, _ in SECTIONS
                                            if name != "dispatch-results") + Counter({"dispatch-results": total})

    code, filename = body("dispatch-results")
    markers = case_markers(code)
    bodies = [dispatch_body("%s/%s" % (index, total))[0].splitlines() for index in range(1, total + 1)]
    assert set(DISPATCH_SECONDS) == set(markers) | {DISPATCH_REMAINDER}
    for case, (begin, end) in markers.items():
        owners = [index for index, part in enumerate(bodies)
                  if all(part[number - 1] == line for number, line in enumerate(code.splitlines(), 1)
                         if begin < number < end)]
        assert len(owners) == 1, (case, owners)
    marked = {number for begin, end in markers.values() for number in range(begin, end + 1)}
    for node in parsed(code, filename).body:
        if node.lineno in marked or isinstance(node, (ast.Import, ast.ImportFrom, ast.FunctionDef,
                                                      ast.AsyncFunctionDef, ast.ClassDef, ast.Assign, ast.AnnAssign)):
            continue
        # Shared declarations may repeat; unmarked assertions and integration
        # calls must still run exactly once, rather than vanish with the cases.
        assert sum(bool(part[node.lineno - 1]) for part in bodies) == 1, node.lineno
    for index in range(1, total + 1):
        extracted, filename = dispatch_body("%s/%s" % (index, total))
        chosen = [case for name, case, bucket in assignments(total)
                  if name == "dispatch-results" and bucket == index]
        lines = {number for case, (begin, end) in markers.items() if case in chosen
                 for number in range(begin + 1, end)}
        if DISPATCH_REMAINDER in chosen:
            lines |= set(range(1, len(code.splitlines()) + 1)) - marked
        bound, loaded = module_names(extracted, filename, lines)
        assert not loaded - bound - provided_names(), (index, sorted(loaded - bound - provided_names()))
    if total == 4:
        assert all("dispatch-results" in selected("%s/4" % index) for index in range(1, 5))
    for costs in ({}, {"unknown case": 0}):
        with patch.dict(globals(), DISPATCH_SECONDS=costs):
            try:
                dispatch_body("1/4")
            except ValueError as error:
                assert str(error) == "dispatch case costs differ from the case manifest", error
            else:
                raise AssertionError("incomplete dispatch case costs accepted")
    for invalid in ("", "0/4", "1/0", "1/5", "5/4", "x/4"):
        try:
            selected(invalid)
        except ValueError:
            pass
        else:
            raise AssertionError("invalid shard accepted: " + invalid)
    shell_source = "# BEGIN shell case\nprintf 'must not execute'\n# END shell case\n"
    with patch.dict(globals(), body=lambda _: (shell_source, "fixture.sh"), self_check=lambda _: None), \
            patch.object(ast, "parse", side_effect=AssertionError("shell case parsed as Python")), \
            patch.object(subprocess, "run", side_effect=AssertionError("shell case launched")), \
            patch("builtins.exec", side_effect=AssertionError("shell case initialized fixtures")):
        for name, _, filename in SECTIONS:
            if not filename or not filename.endswith(".sh"):
                continue
            for mode in ("--run-sections", "--execute"):
                try:
                    main([mode, ".", name, "--case", "shell case"])
                except ValueError as error:
                    assert str(error) == "--case supports Python sections only; run --section %s without --case" % name, error
                else:
                    raise AssertionError("shell case selector was accepted: " + name)
    with tempfile.TemporaryDirectory(prefix="oms-operator-contract-") as directory, \
            patch.dict(os.environ, {}, clear=True), redirect_stdout(io.StringIO()):
        for name, _, filename in SECTIONS:
            if filename and not filename.endswith(".py"):
                continue
            for case in case_markers(body(name)[0]):
                observed = []
                parent = Path(directory) / ("case-%d" % len(list(Path(directory).iterdir())))
                parent.mkdir()
                with patch.object(subprocess, "run", side_effect=lambda command, cwd: observed.append(
                        [part.replace(str(cwd), "{fixture}") for part in command]) or subprocess.CompletedProcess(command, 0)):
                    run_sections([name], parent, case)
                assert observed == [manifests[0][name] + ["--case", case]], (name, case, observed)


def main(args):
    mode = args[0]
    if mode in {"--list", "--self-check"} and len(args) == 2:
        total = int(args[1])
        for name, index in self_check(total):
            print("%s/%s\t%s" % (index, total, name))
    elif mode == "--list-sections" and len(args) == 1:
        self_check(4)
        print("\n".join(name for name, _, _ in SECTIONS))
    elif mode == "--execute" and len(args) in {3, 5} and args[3:4] in ([], ["--case"]):
        name = requested_sections(args[2])[0]
        execute(name, Path(args[1]), args[4] if len(args) == 5 else None)
    elif mode == "--execute-shard" and len(args) == 4 and args[2] == "dispatch-results":
        execute(args[2], Path(args[1]), spec=args[3])
    elif mode == "--run-shard" and len(args) == 3:
        self_check(shard(args[2])[1])
        return run_sections(selected(args[2]), Path(args[1]), spec=args[2])
    elif mode == "--run-sections" and len(args) in {3, 5} and args[3:4] in ([], ["--case"]):
        self_check(4)
        names = requested_sections(args[2])
        case = args[4] if len(args) == 5 else None
        if case is not None:
            if len(names) != 1:
                raise ValueError("--case requires exactly one --section")
            case_body(names[0], case)
        return run_sections(names, Path(args[1]), case)
    else:
        raise ValueError("invalid operator runner arguments")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except (ValueError, StopIteration) as error:
        if sys.argv[1:2] in (["--execute"], ["--execute-shard"]):
            raise
        print("error: " + str(error), file=sys.stderr)
        sys.exit(2)
