#!/usr/bin/env python3
"""One step up the effort ladder, as a printed suggestion; never a launcher.

Shared on purpose: any verb that wants "same brief, one effort step higher"
imports LADDER and next_step instead of keeping its own copy of the order.
A future routing rule table (oms/route-rule-table) would own the ladder; this
stays a tiny pure function so it can be repointed there.
"""
from __future__ import annotations

import shlex
import sys

LADDER = ("low", "medium", "high", "xhigh", "max", "ultra")
# Flags the suggestion rewrites; each takes one value.
REWRITTEN = ("--reasoning-effort", "--workload")


def next_step(effort: str, scale: list[str], workload: str, exact_model: bool):
    """Return (flag, value) for the next step, or None at the top.

    Effort first, limited to what the route accepts (scale). At the top of the
    scale a routine workload moves to the standard one (the only tier peer-delegate
    exposes). An exact --model has no tier to move, so it stops.
    """
    accepted = [level for level in LADDER if level in scale]
    if effort in LADDER:
        higher = [level for level in accepted if LADDER.index(level) > LADDER.index(effort)]
        if higher:
            return ("--reasoning-effort", higher[0])
    elif effort or not accepted:
        return None
    if workload == "routine" and not exact_model:
        return ("--workload", "standard")
    return None


def rerun_argv(argv: list[str], change: tuple[str, str]) -> list[str]:
    kept: list[str] = []
    skip = False
    for word in argv:
        if skip:
            skip = False
        elif word in REWRITTEN:
            skip = True
        else:
            kept.append(word)
    return kept + list(change)


def main(args: list[str]) -> int:
    # Hand-split at "--": the brief's own flags must never reach an option parser.
    head: list[str] = args
    argv: list[str] = []
    if "--" in args:
        cut = args.index("--")
        head, argv = args[:cut], args[cut + 1:]
    options = {"--effort": "", "--scale": "", "--workload": "standard",
               "--command": "oms peer-delegate"}
    for key, value in zip(head[::2], head[1::2]):
        if key in options:
            options[key] = value
    step = next_step(options["--effort"], options["--scale"].split(),
                     options["--workload"], "--model" in argv)
    if step is None:
        print("top")
        return 0
    if step[0] == "--workload":
        # A tier change starts from the tier's own effort, not the old explicit one.
        argv = [word for word in argv if word != "--reasoning-effort"]
    print(f"{step[0]} {step[1]}")
    print(f"{options['--command']} {shlex.join(rerun_argv(argv, step))}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
