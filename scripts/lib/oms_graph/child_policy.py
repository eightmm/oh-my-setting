"""Cooperative authority policy for delegated harness children (W3)."""

from __future__ import annotations

import argparse

CHILD_GRAPH_ERROR = "a harness child may only use read-only graph actions"
READ_ONLY_PROJECT = ("check", "map", "find", "neighbors", "trace", "blast", "analyze")
READ_ONLY_EXEC = ("validate", "render", "route", "status", "events", "test")


def child_action_is_read_only(args: argparse.Namespace) -> bool:
    group = getattr(args, "group", None)
    action = getattr(args, "action", None)
    if group == "project":
        return action in READ_ONLY_PROJECT
    if group == "exec":
        return action in READ_ONLY_EXEC
    return False
