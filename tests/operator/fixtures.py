"""Explicit section fixtures shared by serial, shard and focused invocations."""
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import threading
import uuid
import time
from copy import deepcopy
from contextlib import redirect_stderr
from unittest.mock import Mock, patch

os.environ.pop("NO_COLOR", None)
sys.path.insert(0, str(root / "scripts/lib"))
import terminal_panel as panel
import room
import codex_app_notify
import panel_results
import panel_results as saved
import panel_results as panel_result_store
import panel_debate
import panel_view as question_view
from dashboard_projection import display_width
from panel_view import (render, inbox_items, needs_strip, attention_items, wrapped,
                        use_unicode, render_results, status_tag)
from panel_input import passive_click, TerminalInput, choose
from panel_tree import render_tree
from terminal_panel import tab_event
from room_view import render_graph, plan_rows as render_plan_rows, dock_body_rows
from panel_routing import allocate, policy
from panel_metrics import collect, pane_reading


def fixture_provider_catalog(providers=None):
    rows = [{"provider": name, "binary": name, "installed": True,
             "models": ["fixture/unrouted"] if name == "pi" else []}
            for name in ("claude", "codex", "pi")]
    if providers:
        requested = set(providers.split(",")) if isinstance(providers, str) else set(providers)
        rows = [row for row in rows if row["provider"] in requested]
    return rows


def empty_control(*args, **kwargs):
    kwargs["cache"].update(status=0, state={"collection": {"ok": True}, "attempts": {"active_recent": []}})
    kwargs["navigation"]["main_occupied"] = False
    return 0


def fixture_file(name):
    path = root / "tests/operator" / name
    exec(compile(path.read_text(encoding="utf-8"), str(path), "exec"), globals())


fixture_file("helpers.py")
if section != "core/visual-and-controls":
    fixture_file("provider_fixture.py")
    ledger = project / ".oms/lifecycle/events.jsonl"
    both = {"claude", "codex"}
    fixture_file("pty_fixture.py")


def ended_mains():
    return {owner: call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                        "start", "--provider", owner, "--tool", "panel-main", "--ref", "panel_role=main",
                        "--then", "starting", "--then", "working", "--then", "cancelled"]).strip()
            for owner in panel.PROVIDERS}


def shared_room(native=False):
    global main_attempts, shared, room_owner, room_env
    main_attempts = ended_mains()
    shared = room.create(project, "operator-room", "Shared parser task")
    for owner, attempt in main_attempts.items():
        legacy_main_join(project, shared, attempt, owner, model="gpt-6-sol" if owner == "codex" else "claude-opus-5-5")
        if native:
            panel.events(project, "usage", "--attempt", attempt, "--ref", "panel_room_id=" + shared)
        if not native:
            room.append(project, shared, {"kind": "leave", "participant": attempt}, "Fixture main stopped")
    room_owner = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                       "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                       "--ref", "panel_model=claude-opus-5-5", "--ref", "panel_room_id=" + shared, "--then", "starting", "--then", "working"]).strip()
    legacy_main_join(project, shared, room_owner, "claude", model="claude-opus-5-5", label="Active Opus")
    room_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project),
                    OMS_ROOM_PARTICIPANT=room_owner, OMS_PANEL_MAIN_ATTEMPT=room_owner)


def rendering_fixture():
    # Stable display inputs replace a snapshot of hundreds of earlier CLI calls.
    global shared, child, main_attempts, room_view, tree_room, stamp
    shared = "render-room"
    main_attempts = {"claude": "render-opus", "codex": "render-sol"}
    child = {"participant": "render-worker"}
    members = [
        {"participant": "render-opus", "role": "main", "provider": "claude", "model": "claude-opus-5-5",
         "label": "Active Opus", "joined": True, "seq": 1},
        {"participant": "render-sol", "role": "main", "provider": "codex", "model": "gpt-6-sol",
         "label": "Previous Sol", "joined": True, "seq": 2},
        {"participant": "render-worker", "role": "worker", "provider": "codex", "model": "gpt-6-luna",
         "label": "Inspect", "parent": "render-opus", "joined": True, "seq": 3},
        {"participant": "render-advisor", "role": "advisor", "provider": "codex", "model": "gpt-6-astra",
         "label": "Source review", "parent": "render-opus", "joined": True, "seq": 4}]
    attempts = [{"attempt_id": m["participant"], "state": "working" if m["participant"] == "render-opus" else "done",
                 "panel": {"role": m["role"], "room_id": shared, "room_participant": m["participant"],
                           "parent": m.get("parent", ""), "model": m["model"]}}
                for m in members]
    room_view = {"repo": {"name": "render-fixture"}, "collection": {"ok": True},
                 "room": {"id": shared, "title": "Shared parser task", "participants": members,
                          "messages": [], "call_results": {"render-worker": {"exit": 0}, "render-advisor": {"exit": 0}}},
                 "attempts": {"active_recent": attempts[:1], "recent": attempts[1:]},
                 "operations": {"recent": []}}
    tree_room = deepcopy(room_view)
    stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def minutes_ago(minutes):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - minutes * 60))


if section == "dispatch-results":
    inventory = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--providers", "--json"]))
    registered = {row["provider"]: row for row in inventory["providers"]}
elif section == "native-pty":
    main_attempts = ended_mains()
elif section in {"dispatch-exit", "auto-review", "core/native-integration"}:
    shared_room(native=section == "core/native-integration")
    if section == "auto-review":
        (project / ".oms/plan").mkdir(parents=True, exist_ok=True)
    elif section == "core/native-integration":
        # One read result supplies the native Past-tab/PTY integration fixture.
        call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
              "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate",
              "--workload", "light", "--prompt", "Inspect value.txt without edits.",
              "--task-id", "native-fixture", "--label", "Native fixture"], room_env)
        room_view = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                     "--room", shared, "--json"], room_env))["dashboard"]
        child = next(member for member in room_view["room"]["participants"] if member["role"] == "worker")
        tree_room = deepcopy(room_view)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
elif section == "core/room-and-board-rendering":
    rendering_fixture()

if section == "core/visual-and-controls":
    active = {"repo": {"name": "operator fixture"}, "collection": {"ok": True},
              "attempts": {"active_recent": [{"attempt_id": "att_still_active", "state": "queued",
                  "provider": "codex", "tool": "agent-supervisor", "task_id": "panel-ui",
                  "panel": {"role": "researcher", "harness": "codex", "room_id": "research-room",
                            "room_participant": "research-call"}}], "recent": []},
              "delegations": [{"id": "live", "task_id": "panel-ui", "provider": "codex", "live": True,
                               "requested_model": "model-for-live-worker", "reasoning_effort": "high",
                               "state": "running"}], "operations": {"recent": []},
              "acceptance": {"total": 2, "counts": {"missing": 2}, "complete": False},
              "plan": {"by_state": {"ready": 1, "review": 1}}, "coverage": {}}
    (temporary / "panel-active.json").write_text(json.dumps(active), encoding="utf-8")
