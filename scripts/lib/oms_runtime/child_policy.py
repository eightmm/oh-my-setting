"""Cooperative authority policy for delegated OMS runtime children."""

from __future__ import annotations

import argparse


CHILD_RUNTIME_ERROR = "a harness child may only use read-only runtime actions"


def child_action_is_read_only(args: argparse.Namespace) -> bool:
    """Return whether a parsed runtime action stays on observation surfaces.

    The allowlist is intentionally semantic and fail-closed: argparse has
    already resolved option ordering and defaults, and every future action is
    denied to a marked child until it is classified here.
    """
    command = getattr(args, "command", None)
    if command == "envelope":
        return getattr(args, "action", None) == "show"
    if command == "evidence":
        return getattr(args, "evidence_action", None) in ("show", "unbound")
    if command == "next":
        return True
    if command == "failure":
        return getattr(args, "failure_action", None) in ("classify", "catalog")
    if command == "context":
        return False
    if command == "profile":
        action = getattr(args, "profile_action", None)
        if action in ("list", "current", "show", "check", "install-plan"):
            return True
        return action == "install" and bool(getattr(args, "dry_run", False))
    if command == "release":
        action = getattr(args, "release_action", None)
        if action == "status":
            return True
        return action == "resolve" and not bool(getattr(args, "fetch", False))
    if command == "capsule":
        return getattr(args, "capsule_action", None) in ("verify", "diff")
    if command == "backend":
        return getattr(args, "backend_action", None) in ("describe", "check")
    if command == "experiment":
        action = getattr(args, "experiment_action", None)
        if action == "template":
            return not bool(getattr(args, "output", ""))
        return action in ("validate", "compare", "show", "summarize", "evaluate")
    if command == "benchmark":
        return getattr(args, "benchmark_action", None) in ("show", "compare")
    if command == "doctor":
        return True
    return False
