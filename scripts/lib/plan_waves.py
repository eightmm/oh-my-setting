"""Wave numbers for plan tasks: which could run side by side if anything ran in parallel.

A reading aid for the person approving a proposal or looking at a plan. Nothing
schedules from it: plan-run still claims one ready task at a time.

A task's wave is one more than the latest wave among the tasks it depends on,
then pushed later until no task already in that wave may write the same paths.
Tasks are placed in the order given, which is the order a proposal lists them
and a plan stores them, so the result is deterministic.
"""

GLOB = "*?["


def _root(pattern):
    """The literal directory prefix a scope entry certainly stays under; "" means anywhere."""
    text = str(pattern).strip().replace("\\", "/")
    for index, char in enumerate(text):
        if char in GLOB:
            text = text[:index].rsplit("/", 1)[0] if "/" in text[:index] else ""
            break
    text = text.strip("/")
    while text.startswith("./"):
        text = text[2:]
    return "" if text in ("", ".") else text


def overlaps(left, right):
    """True when two tasks' allowed paths could name the same file. No scope at all means anywhere."""
    lefts = [_root(item) for item in left or [""]]
    rights = [_root(item) for item in right or [""]]
    for a in lefts:
        for b in rights:
            if not a or not b or a == b or a.startswith(b + "/") or b.startswith(a + "/"):
                return True
    return False


def waves(tasks):
    """{task id: wave number from 1} for tasks given as dicts with id, depends and allowed paths.

    `allowed_paths` (a stored plan task) or `allowed` (a proposal task) is read. A dependency
    that is not among the given tasks is already finished and adds no wait."""
    placed, scopes, by_wave = {}, {}, {}
    for task in tasks:
        if not isinstance(task, dict) or not isinstance(task.get("id"), str):
            continue
        ident = task["id"]
        scope = task.get("allowed_paths")
        if scope is None:
            scope = task.get("allowed")
        scope = [item for item in scope if isinstance(item, str)] if isinstance(scope, list) else []
        wave = 1 + max([placed.get(dep, 0) for dep in task.get("depends") or [] if isinstance(dep, str)] or [0])
        while any(overlaps(scope, scopes[other]) for other in by_wave.get(wave, [])):
            wave += 1
        placed[ident] = wave
        scopes[ident] = scope
        by_wave.setdefault(wave, []).append(ident)
    return placed


def summary(tasks):
    """[[ids of wave 1], [ids of wave 2], ...] in placement order."""
    placed = waves(tasks)
    rows = [[] for _ in range(max(placed.values() or [0]))]
    for ident, wave in placed.items():
        rows[wave - 1].append(ident)
    return rows
