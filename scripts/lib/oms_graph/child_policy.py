"""Cooperative authority policy for delegated harness children (W3)."""

from __future__ import annotations

import argparse

CHILD_GRAPH_ERROR = "a harness child may only use read-only graph actions"
# `build` and `ensure` join the readers: the project graph is a regenerable
# cache that carries no authority, and a delegated worker in an isolated
# worktree has no other way to get one. `context` writes a pack the parent's
# brief owns, so it stays parent-only alongside every exec writer.
CHILD_PROJECT_ACTIONS = ("build", "ensure", "check", "map", "find", "neighbors", "trace", "blast", "analyze", "coupling")
CHILD_EXEC_ACTIONS = ("validate", "render", "route", "status", "events", "test")


def child_action_is_allowed(args: argparse.Namespace) -> bool:
    group = getattr(args, "group", None)
    action = getattr(args, "action", None)
    if group == "project":
        return action in CHILD_PROJECT_ACTIONS
    if group == "exec":
        return action in CHILD_EXEC_ACTIONS
    return False
