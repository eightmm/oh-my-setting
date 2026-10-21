assert room_view["room"]["id"] == shared
room_view["attempts"]["active_recent"].append({"attempt_id": "graph-example-child", "state": "working",
    "task_id": "graph-sample", "panel": {"role": "worker", "room_id": shared,
    "room_participant": child["participant"], "model": "gpt-6-luna", "label": "긴 작업 설명을 분리해서 표시",
    "location": "worktree:example/wt"}})
immutable = json.dumps(room_view, sort_keys=True)
graph = render(room_view, "claude", 100, 44, view="graph", frame=0)
assert graph.startswith("OMS · ") and "unread for live participants" not in graph and "MAIN " in graph and "Luna" in graph, graph
assert "No joined worker" not in graph and "No joined judge" not in graph, graph
degraded = deepcopy(room_view)
degraded["collection"] = {"ok": False}
degraded["room"]["error"] = "Room evidence unavailable"
for width, height in ((100, 44), (76, 36), (35, 24), (35, 23), (35, 12)):
    for density in ("graph", "compact"):
        for menu in (False, True):
            drawn = render(degraded, "claude", width, height, view=density, menu=menu)
            assert ("State unavailable" if density == "graph" and width >= 76 else "Collection degraded") in drawn and "Room evidence unavailable" in drawn, drawn
            assert len(drawn.splitlines()) <= height - int(menu)
            assert all(display_width(line) <= width for line in drawn.splitlines())
assert graph == render(room_view, "claude", 100, 44, view="graph", frame=1)
assert json.dumps(room_view, sort_keys=True) == immutable
fallback_nav = {}
fallback = render(room_view, "claude", 35, 23, view="graph", navigation=fallback_nav)
assert fallback_nav["surface"] == "tree" and fallback_nav["hits"], fallback
assert "Graph needs" not in fallback and "OMS dashboard" not in fallback, fallback
main_states = deepcopy(room_view)
main_states["room"]["participants"] = [
    {"participant": "main-live", "provider": "codex", "model": "gpt-6-sol", "role": "main",
     "label": "Live main", "joined": True, "seq": 1},
    {"participant": "main-exited", "provider": "claude", "model": "opus", "role": "main",
     "label": "Exited main", "joined": True, "seq": 2}]
main_states["attempts"]["active_recent"] = [{"attempt_id": "live-attempt", "state": "working",
    "panel": {"role": "main", "room_id": main_states["room"]["id"], "room_participant": "main-live"}}]
main_states["attempts"]["recent"] = [{"attempt_id": "exited-attempt", "state": "failed",
    "panel": {"role": "main", "room_id": main_states["room"]["id"], "room_participant": "main-exited"}}]
# The narrow board dashboard names one live main; exited predecessors remain past.
mixed_mains = render(main_states, "claude", 27, 30, view="graph")
assert "Main Sol 6" in mixed_mains and "Joined mains" not in mixed_mains, mixed_mains
# A joined predecessor with no attempt evidence never displaces the working main when no owner is given.
unknown_first = deepcopy(main_states)
unknown_first["room"]["participants"].insert(0, {"participant": "main-unknown", "provider": "claude", "model": "opus",
    "role": "main", "label": "Unknown main", "joined": True, "seq": 0})
assert "Main Sol 6" in render(unknown_first, "claude", 27, 30, view="graph"), render(unknown_first, "claude", 27, 30, view="graph")
compact_dashboard = deepcopy(main_states)
compact_dashboard["attempts"]["active_recent"] = []
compact_dashboard["attempts"]["recent"] = []
compact_dashboard["room"]["participants"] = [m for m in compact_dashboard["room"]["participants"] if m["participant"] == "main-live"]
compact_dashboard["goal"] = {"text": "Keep the narrow board useful"}
compact_dashboard["acceptance"] = {"available": True, "complete": False, "total": 4, "counts": {"verified": 2}}
compact_dashboard["room"].update({"title": "Visual team", "pending_count": 3, "received_count": 8, "answered_count": 5})
for columns in (19, 22, 27):
    board = render(compact_dashboard, "claude", columns, 30, view="graph")
    assert "Graph needs" not in board and "Finished is not accepted" not in board and "Native agents are read-only" not in board, board
    assert all(fact in board for fact in ("Room: Visual team", "Main Sol 6", "Unread 3", "Next: Claude",
                                          "Goal: Keep", "Accept 2/4")), board
    assert len(board.splitlines()) <= 30 and all(display_width(line) <= columns for line in board.splitlines()), board
# Claude Code's own subagents render as read-only rows that fit narrow and wide boards in both glyph sets.
native_report = deepcopy(main_states)
native_report["room"]["participants"] = [{"participant": "main-claude", "provider": "claude", "model": "opus",
                                          "role": "main", "label": "Claude main", "joined": True, "seq": 1}]
native_report["attempts"].update(active_recent=[{"attempt_id": "claude-attempt", "state": "working", "panel": {
    "role": "main", "room_id": native_report["room"]["id"], "room_participant": "main-claude"}}], recent=[])
active = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 120))
native_report["native_subagents"] = {"main-claude": {
    "agents": [{"id": "r1", "title": "Audit the parser with a very long description that must clip", "active": active,
                "tokens": 2465, "output": 450, "partial": False}],
    "workflows": [{"id": "wf_x", "title": "Workflow wf_x · phase Find · agents ~4 started · ~1 done", "active": active,
                   "agents": 5, "live": 3, "tokens": 120000, "output": 20000, "partial": True}]}}
for columns, rows in ((76, 30), (110, 44)):
    for glyphs in (True, False):
        native_screen = render(native_report, "claude", columns, rows, view="tree", unicode=glyphs)
        assert "SUBAGENTS (2)" in native_screen, native_screen
        assert "2465 tok (out 450)" in native_screen and "3/5 agents live" in native_screen, native_screen
        assert "120.0k+ tok (out 20.0k)" in native_screen, native_screen
        assert all(display_width(line) <= columns for line in native_screen.splitlines()), native_screen
# A window whose main is not the Claude main shows none of that main's agents (not even as unlinked calls).
cross_report = deepcopy(native_report)
cross_report["room"]["participants"].append({"participant": "main-codex", "provider": "codex", "model": "gpt-6-sol",
                                             "role": "main", "label": "Codex main", "joined": True, "seq": 2})
cross_report["attempts"]["active_recent"].append({"attempt_id": "codex-attempt", "state": "working", "panel": {
    "role": "main", "room_id": native_report["room"]["id"], "room_participant": "main-codex"}})
assert "SUBAGENTS" not in render(cross_report, "codex", 110, 44, view="tree", main_attempt="codex-attempt")
assert "SUBAGENTS (2)" in render(cross_report, "claude", 110, 44, view="tree", main_attempt="claude-attempt")
boundary_dashboard = render(compact_dashboard, "claude", 28, 30, view="graph")
board = render(dict(compact_dashboard, room=dict(compact_dashboard["room"], predecessor="room-earlier")), "claude", 60, 30, view="graph")
assert "ROOM / Visual team (continues room-earlier)" in board, board
assert "Graph needs" in boundary_dashboard and "REPO ACCEPT" in boundary_dashboard, boundary_dashboard
import panel_health
unknown_capacity = panel_health.render({"collection": {"ok": False}, "room": {"participants": [
    {"participant": "unknown-main", "provider": "claude", "role": "main", "state": "working", "joined": True}]}}, 80, 20)
assert "Claude main: context [unknown]" in unknown_capacity, unknown_capacity
main_states["attempts"]["active_recent"] = []
main_states["attempts"]["recent"].append({"attempt_id": "live-exited-attempt", "state": "cancelled",
    "panel": {"role": "main", "room_id": main_states["room"]["id"], "room_participant": "main-live"}})
exited_mains = render(main_states, "claude", 27, 30, view="graph")
assert "Joined mains" not in exited_mains and "Main 0 live / 2 exited" in exited_mains, exited_mains
for source_view, dimensions in ((room_view, ((80, 11), (80, 12), (60, 14), (100, 16))),
                                (degraded, ((100, 30),))):
    for columns, rows in dimensions:
        nav = {}
        screen = render(source_view, "claude", columns, rows, view="graph", navigation=nav)
        assert any(item[0] == "chat" for item in nav["items"]), (columns, rows, nav, screen)
        assert any(item[0] == "result" for item in nav["items"]), (columns, rows, nav, screen)
        assert any(hit["action"][0] == "chat" for hit in nav["hits"]), (columns, rows, nav, screen)
        assert any(hit["action"][0] == "result" for hit in nav["hits"]), (columns, rows, nav, screen)
        assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines())
        if source_view is degraded:
            assert "State unavailable" in screen and "Room evidence unavailable" in screen
for columns, rows in ((100, 44), (76, 36), (60, 36), (36, 28), (2, 8)):
    for ascii_only in (False, True):
        drawn = render(room_view, "claude", columns, rows, view="graph", unicode=not ascii_only, menu=True)
        assert len(drawn.splitlines()) <= rows - 1 and all(display_width(line) <= columns for line in drawn.splitlines()), drawn
        assert "\033" not in drawn
        if ascii_only:
            assert "╭" not in drawn and "─" not in drawn
# The tree roots one live main, maps cells from the drawn frame, and
# never manufactures a parent or changes room data while folding/scrolling.
from panel_input import TerminalInput, choose
from panel_metrics import cached, codex_weekly_reading, collect, pane_reading, percent, usage_line
tree_room = deepcopy(room_view)
tree_room["room"]["participants"].extend([
    {"participant": "extra-main", "provider": "codex", "model": "gpt-6-sol", "role": "main",
     "label": "Third main", "joined": True, "seq": 100},
    {"participant": "unlinked-worker", "provider": "codex", "model": "gpt-6-luna", "role": "worker",
     "label": "연결 근거 없는 작업", "joined": True, "seq": 101, "parent": "outside-room"}])
before_tree = json.dumps(tree_room, sort_keys=True)

layout_tree = deepcopy(tree_room)
layout_main = "layout-main"
layout_room_id = layout_tree["room"]["id"]
layout_members = [{"participant": layout_main, "provider": "codex", "model": "gpt-6-sol", "role": "main",
                   "label": "Layout main", "joined": True, "seq": 1}]
layout_attempts = [{"attempt_id": "layout-main-attempt", "state": "working", "panel": {
    "room_id": layout_room_id, "room_participant": layout_main, "role": "main"}}]
for ident, role, state in (("layout-review-a", "reviewer", "working"),
                           ("layout-review-b", "reviewer", "working"),
                           ("layout-builder", "worker", "working"),
                           ("layout-awaiting", "worker", "review")):
    layout_members.append({"participant": ident, "provider": "codex", "model": "gpt-6-luna", "role": role,
                           "label": ident, "parent": layout_main, "joined": True, "seq": len(layout_members) + 1})
    layout_attempts.append({"attempt_id": ident, "state": state, "panel": {
        "room_id": layout_room_id, "room_participant": ident, "role": role}})
layout_tree["room"]["participants"] = layout_members
layout_tree["attempts"] = {"active_recent": layout_attempts, "recent": []}

# The visual flow selects one exact parent, keeps an ended main in history, and
# uses real role cards with frame-local chat/result targets.
flow_members = [{"participant": "flow-main", "role": "main", "joined": True, "provider": "codex",
                 "model": "gpt-6-sol", "label": "Main native chat"},
                {"participant": "flow-other", "role": "main", "joined": True, "provider": "claude",
                 "model": "claude-opus-5-5", "label": "Other native chat"}]
for ident, role, model in (("advisor", "advisor", "gpt-6-astra"), ("reviewer", "reviewer", "claude-fable-5-1"),
                          ("researcher", "worker", "gpt-6-luna"), ("explorer", "worker", "gpt-6-luna"),
                          ("builder", "worker", "claude-sonnet-5-5")):
    flow_members.append({"participant": ident, "role": role, "model": model, "joined": True,
                         "provider": "codex", "parent": "flow-attempt", "label": "한글 작업 제목 / " + ident})
flow_members += [{"participant": "foreign-child", "role": "worker", "model": "gpt-6-luna", "joined": True,
                  "parent": "flow-other", "label": "ONLY OTHER OWNER"},
                 {"participant": "unlinked-child", "role": "worker", "joined": True,
                  "parent": "missing-owner", "label": "NO PARENT PROOF"}]
flow = {"collection": {"ok": True}, "room": {"id": "flow-room", "title": "Visual team", "participants": flow_members},
        "attempts": {"active_recent": [{"attempt_id": "flow-attempt" if m["participant"] == "flow-main" else m["participant"],
            "state": "done" if m["participant"] == "flow-other" else "working",
            "panel": {"room_id": "flow-room", "room_participant": m["participant"], "role": m["role"]}}
            for m in flow_members]}, "operations": {"recent": [{"kind": "call", "event_id": model,
                "served_model": model, "tokens": 10} for model in ("gpt-6-sol", "gpt-6-astra", "gpt-6-luna")]}}
flow_before = json.dumps(flow, sort_keys=True)
two_main_cards = render(flow, "codex", 157, 50, view="graph", navigation={"dismissed": True})
assert "MAIN Sol 6" in two_main_cards and "Other native chat" not in two_main_cards, two_main_cards
overview_nav = {"dismissed": True}
overview_graph = render(flow, "codex", 124, 40, view="graph", navigation=overview_nav)
assert overview_nav["surface"] == "graph", overview_nav
capped_nav = {"dismissed": True}
capped_tree = render(flow, "codex", 60, 40, view="graph", navigation=capped_nav)
assert capped_nav["surface"] == "tree" and ("chat", "flow-main") in capped_nav["items"], capped_nav
single_flow = deepcopy(flow)
single_flow["room"]["participants"] = [m for m in single_flow["room"]["participants"] if m["participant"] not in {"flow-other", "foreign-child", "unlinked-child"}]
for glyphs in (False, True):
    one_nav = {"dismissed": True}
    one_cards = render(single_flow, "codex", 80, 40, view="graph", unicode=glyphs, navigation=one_nav)
    assert " for " not in one_cards and "WORKER / " not in one_cards and "WRK" not in one_cards, one_cards
    for ident, title in (("researcher", "WORKER Luna 6"), ("builder", "WORKER Sonnet 5.5")):
        edge = next(hit for hit in one_nav["hits"] if hit["action"] == ("result", ident))
        assert title in one_cards.splitlines()[edge["y"] - 1], one_cards
# An exited predecessor leaves one live main: its calls drop "for <main>"; the predecessor's calls keep it.
successor_flow = deepcopy(flow)
next(a for a in successor_flow["attempts"]["active_recent"] if a["attempt_id"] == "flow-other")["state"] = "done"
for glyphs in (False, True):
    successor_cards = render(successor_flow, "codex", 127, 40, view="graph", unicode=glyphs, navigation={"dismissed": True})
    assert " for " not in successor_cards and "running" in successor_cards, successor_cards
    predecessor_cards = render(successor_flow, "codex", 127, 40, view="graph", unicode=glyphs,
                               navigation={"dismissed": True, "selected": ("result", "foreign-child")})
    assert "MAIN Opus 5.5" in predecessor_cards and "ONLY OTHER OWNER" in predecessor_cards, predecessor_cards
# A sole main stays the primary in every non-terminal state, so its running worker stays a target.
for main_state in ("blocked", "waiting_input", "presence unknown"):
    held = deepcopy(single_flow)
    held["attempts"]["active_recent"] = [a for a in held["attempts"]["active_recent"] if a["attempt_id"] != "flow-attempt"]
    if main_state == "presence unknown":
        for member in held["room"]["participants"]:
            member["parent"] = "flow-main" if member.get("parent") == "flow-attempt" else member.get("parent")
    else:
        held["attempts"]["active_recent"].append({"attempt_id": "flow-attempt", "state": main_state,
            "panel": {"room_id": "flow-room", "room_participant": "flow-main", "role": "main"}})
    # A failed predecessor ordered first never takes the lead from the held current main.
    preceded = deepcopy(held)
    preceded["room"]["participants"].insert(0, deepcopy(next(m for m in flow_members if m["participant"] == "flow-other")))
    next(a for a in preceded["attempts"]["active_recent"] if a["attempt_id"] == "flow-other")["state"] = "failed"
    for report, view, columns, rows in [(r, ) + size for r in (held, preceded) for size in (("graph", 127, 40), ("tree", 42, 24))]:
        held_nav = {"dismissed": True}
        render(report, "codex", columns, rows, view=view, navigation=held_nav)
        assert {("chat", "flow-main"), ("result", "builder")} <= set(held_nav["items"]), (main_state, view, held_nav)
# An ended main's unfinished calls stay visible beside the live main and selectable in the tree.
gone_flow = deepcopy(successor_flow)
gone_flow["room"]["participants"] = [m for m in gone_flow["room"]["participants"] if m["participant"] != "unlinked-child"]
gone_board = render(gone_flow, "codex", 157, 50, view="graph", navigation={"dismissed": True})
assert "1 call of an ended main" in gone_board, gone_board
for columns, rows in ((42, 24), (157, 50)):
    gone_nav = {"dismissed": True}
    render(gone_flow, "codex", columns, rows, view="tree", navigation=gone_nav)
    assert ("result", "foreign-child") in gone_nav["items"] and ("chat", "flow-other") not in gone_nav["items"], gone_nav
# A predecessor with no attempt evidence is not shown live either, so the live main's calls still drop "for <main>".
unproven_flow = deepcopy(flow)
unproven_flow["attempts"]["active_recent"] = [a for a in flow["attempts"]["active_recent"] if a["attempt_id"] != "flow-other"]
from room_view import nodes as room_nodes
assert next(m for m in room_nodes(unproven_flow) if m["participant"] == "flow-other")["state"] == "presence unknown"
for glyphs in (False, True):
    unproven_cards = render(unproven_flow, "codex", 127, 40, view="graph", unicode=glyphs, navigation={"dismissed": True})
    assert " for " not in unproven_cards and "running" in unproven_cards, unproven_cards
# Every call is one tree row under the main in either glyph set; the selected one is drawn and opens on click.
geometry_flow = deepcopy(flow)
for attempt in geometry_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "waiting_approval"
for columns, rows, menu in ((34, 50, False), (56, 50, False), (76, 16, False), (76, 16, True), (76, 24, False), (100, 24, False), (127, 50, False), (157, 66, False)):
    for glyphs in (False, True):
        geometry_nav = {"dismissed": True, "selected": ("result", "builder")}
        geometry_picture = render(geometry_flow, "codex", columns, rows, view="graph", unicode=glyphs,
                                  main_attempt="flow-attempt", navigation=geometry_nav, menu=menu)
        geometry_lines = geometry_picture.splitlines()
        assert len(geometry_lines) <= rows - int(menu) and all(display_width(line) <= columns for line in geometry_lines)
        assert "Sonnet 5.5" in geometry_picture and "approval" in geometry_picture, geometry_picture
        if columns in (34, 56):
            assert geometry_nav["surface"] == "tree" and geometry_nav["tab_strip_visible"], geometry_nav
            assert any(h["action"] == ("tab", "messages") for h in geometry_nav["hits"]), geometry_nav
            assert tab_event(("tab",), geometry_nav), geometry_nav
        if geometry_nav.get("surface") != "graph":
            assert any(hit["action"] == ("result", "builder") for hit in geometry_nav["hits"]), geometry_nav
            continue
        tree_rows = [hit for hit in geometry_nav["hits"] if hit.get("tree")]
        drawn_calls = [hit["action"][1] for hit in tree_rows if "more " not in geometry_lines[hit["y"] - 1]]
        assert "builder" in drawn_calls and drawn_calls[-1] == "builder", (geometry_picture, drawn_calls)
        for ident in drawn_calls:
            edge = next(hit for hit in tree_rows if hit["action"] == ("result", ident))
            row = geometry_lines[edge["y"] - 1]
            assert display_width(row) == columns and edge["x1"] == 1 and edge["x2"] == columns, (row, edge)
            assert row.strip()[0] in ("┌├└╭" if glyphs else "+`,"), geometry_picture
            if ident == "builder":
                assert ("╌ ▸ " if glyphs else "- > ") in row and "Sonnet 5.5" in row and "needs approval" in geometry_picture, row
                assert columns < 100 or "한글 작업 제목" in row, row
            geometry_click = dict(geometry_nav, selected=None, preview={})
            assert choose(("click", 1, edge["y"]), geometry_click) is None
            assert geometry_click["selected"] == ("result", ident)
            assert geometry_click["preview"]["target"] == ("result", ident)
import panel_chats
fast_room = deepcopy(flow["room"])
fast_room["participants"][0].update(consumer="enrolled-main-hash", provider="codex")
fast_room["participants"][1].update(consumer="other-main-hash", provider="claude")
target = {"session": "$1", "window": "@1", "pane": "%1"}
with patch.object(panel_chats.room, "status", return_value=fast_room), \
        patch.object(panel_chats, "windows", return_value={"flow-main": [target]}), \
        patch.object(panel_chats, "native_index", side_effect=AssertionError("unneeded transcript scan")), \
        patch.object(panel_chats, "terminal_commands", return_value=[["tmux", "select-pane", "-t", "%1"]]):
    assert panel_chats.plan(Path("."), "flow-room", "flow-main")["method"] == "existing-terminal"
for columns, rows in ((110, 44), (100, 24), (76, 18), (76, 21)):
    for alert in (None, "! Capacity full", "! Collection degraded - some state unknown"):
        graph_report = deepcopy(flow)
        if alert == "! Capacity full":
            graph_report["capacity_alerts"] = [alert]
        elif alert:
            graph_report["collection"]["ok"] = False
        for glyphs in (False, True):
            for menu in (False, True):
                nav = {}
                picture = render(graph_report, "codex", columns, rows, view="graph", unicode=glyphs, menu=menu,
                                 main_attempt="flow-attempt", navigation=nav)
                assert nav["surface"] == "graph", (alert, nav)
                if alert:
                    assert ("State unavailable" if "Collection" in alert else alert) in picture, picture
                # Calls hang below the main; a short board folds the rows it cannot draw.
                assert "MAIN Sol 6" in picture and "ADVISOR Astra" in picture, picture
                assert picture.index("ADVISOR Astra") < picture.index("MAIN Sol 6"), picture
                assert "WORKER Luna" in picture or "more workers" in picture, picture
                assert "ONLY OTHER OWNER" not in picture and "NO PARENT PROOF" not in picture, picture
                assert len(picture.splitlines()) <= rows - int(menu), picture
                assert all(display_width(line) <= columns for line in picture.splitlines()), picture
                target = next(hit for hit in nav["hits"] if hit["action"] == ("result", "builder"))
                # A call's content shows in the board's detail area; clicking again opens no other screen.
                assert choose(("click", target["x1"], target["y"]), nav) is None
                assert nav["preview"] == {"target": ("result", "builder")}
                assert choose(("click", target["x1"], target["y"]), nav) is None
                nav["surface"] = "graph"
                assert choose(("enter",), nav) is None and nav["preview"]["target"] == ("result", "builder")
                nav.pop("preview")
                # Keys reach only this board's main and its calls, never an earlier main's.
                assert all(item[1] not in ("flow-other", "foreign-child") for item in nav["items"]), nav["items"]
                choose(("end",), nav)
                render(graph_report, "claude", columns, rows, view="graph", unicode=glyphs, menu=menu,
                       main_attempt="flow-attempt", navigation=nav)
                assert any(hit["action"] == nav["selected"] for hit in nav["hits"]), nav
                if not glyphs:
                    assert "╭" not in picture and "─" not in picture
assert render(flow, "codex", 100, 24, view="graph", frame=0) == render(flow, "codex", 100, 24, view="graph", frame=1)
# The tree roots this window's live main; an ended predecessor's call stays attributable to history.
own_root_nav = {"dismissed": True}
tree = render(flow, "codex", 110, 40, view="tree", main_attempt="flow-attempt", navigation=own_root_nav)
assert "Main native chat" in tree and "Other native chat" not in tree, tree
assert {item for item in own_root_nav["items"] if item[0] == "chat"} == {("chat", "flow-main")}, own_root_nav
assert ("result", "foreign-child") in own_root_nav["items"], own_root_nav
# An ended predecessor on the same model keeps its id tail in historical detail.
twin = deepcopy(flow)
twin["room"]["participants"].append({"participant": "twin-main-9f3c", "role": "main", "joined": True, "provider": "claude",
                                     "model": "claude-opus-5-5", "label": "Twin native chat"})
twin["attempts"]["active_recent"].append({"attempt_id": "twin-main-9f3c", "state": "done", "panel": {
    "room_id": "flow-room", "room_participant": "twin-main-9f3c", "role": "main"}})
from room_view import main_names, nodes
twin_names = main_names(nodes(twin))
assert twin_names["twin-main-9f3c"] == "Opus 5.5 #9f3c" and twin_names["flow-other"] == "Opus 5.5", twin_names
twin_detail = render(twin, "codex", 157, 40, view="graph", navigation={"selected": ("result", "foreign-child"),
    "tab": "detail", "preview": {"target": ("result", "foreign-child"), "report": {}}})
assert "MAIN Opus 5.5" in twin_detail and "ONLY OTHER OWNER" in twin_detail, twin_detail
# Wheel input scrolls only the band under the pointer: the folded call tree, not the detail box.
crowded = deepcopy(flow)
crowded["room"]["participants"] += [{"participant": "extra-%s" % n, "role": "worker", "model": "gpt-6-luna",
    "joined": True, "parent": "flow-attempt", "label": "EXTRA WORKER %s" % n} for n in range(3)]
crowded["attempts"]["active_recent"] += [{"attempt_id": "extra-%s" % n, "state": "working", "panel": {
    "room_id": "flow-room", "room_participant": "extra-%s" % n, "role": "worker"}} for n in range(3)]
nav = {"tab": "detail"}
render(crowded, "codex", 110, 22, view="graph", main_attempt="flow-attempt", navigation=nav)
lower = next(b for b in nav["bands"] if b["name"] == "calls")
assert "more workers" in render(crowded, "codex", 110, 22, view="graph", main_attempt="flow-attempt", navigation=nav)
choose(("scroll", 3, 5, lower["y1"]), nav)
assert nav["band_offsets"]["calls"] == 1 and nav["band_offsets"]["detail"] == 0, nav["band_offsets"]
scrolled_tree = render(crowded, "codex", 110, 22, view="graph", main_attempt="flow-attempt", navigation=nav)
assert "EXTRA WORKER" in scrolled_tree and "ADVISOR Astra" in scrolled_tree, scrolled_tree
assert nav["tab_body_visible"] and nav["band_offsets"]["calls"] == 1, nav
nav["selected"] = ("result", "extra-2")
for unused in range(2):
    render(crowded, "codex", 110, 22, view="graph", main_attempt="flow-attempt", navigation=nav)
    # Choosing a drawn row keeps the tree where it is.
    assert nav["band_offsets"]["calls"] >= 1, nav["band_offsets"]
    assert any(h["action"] == nav["selected"] and h["y"] < nav["box"]["y1"] for h in nav["hits"]), nav
# The overview keeps one-line cards while the selected block's content fills the lower detail area.
mailed = deepcopy(flow)
mailed["room"]["messages"] = [{"sender": "advisor", "targets": ["flow-main"], "text": "MAIL FOR MAIN"}]
mail_before = json.dumps(mailed, sort_keys=True)
detail_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
picture = render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation=detail_nav)
assert "[Detail]" in picture and "MAIN Sol 6" in picture and "Team:" not in picture, picture
mail_last = render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt",
                   navigation={"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}, "band_offsets": {"detail": 99}})
assert "MAIL FOR MAIN" not in mail_last and "Workers running:" in mail_last, mail_last
assert "ADVISOR Astra" in picture and len(picture.splitlines()) <= 30, picture
assert json.dumps(mailed, sort_keys=True) == mail_before, "reading main detail changed room mail"
detail_band = next(b for b in detail_nav["bands"] if b["name"] == "detail")
choose(("scroll", 3, 5, detail_band["y1"] + 1), detail_nav)
assert detail_nav["band_offsets"]["detail"] == 3, detail_nav["band_offsets"]
# Plain Detail text is passive: repeated clicks neither navigate nor reset; a border row still activates the card.
body = next(h for h in detail_nav["hits"] if h.get("passive"))
for unused in range(2):
    assert choose(("click", body["x1"], body["y"]), detail_nav) is None
assert detail_nav["preview"]["target"] == ("chat", "flow-main") and "report" in detail_nav["preview"], detail_nav["preview"]
assert not any(h.get("passive") for h in detail_nav["hits"] if h["y"] in (detail_band["y1"], detail_band["y2"]))
# An ordinary report keeps its body; only a deliberation (Answer/Findings) drops text before its sections.
import panel_results
report_artifact = (b"# Peer call\n## Output\n**What changed:** BODY TEXT\n**Why / Evidence:** FINDING TEXT\n"
                   b"**Verification:** passed\n## Exit\n0\n")
with patch.object(panel_results, "_records", return_value=(
        [{"attempt_id": "a1", "task_id": "report-body", "state": "done", "refs": {}}],
        [{"kind": "ask", "task_id": "report-body", "attempt_id": "a1", "artifact": ".oms/artifacts/ask/r.md", "ts": "1"}])), \
        patch.object(panel_results, "_indexed", return_value=report_artifact):
    kept_answer = panel_results.results(project, task_id="report-body")["rows"][0]["calls"][0]["answer"]
assert "BODY TEXT" in kept_answer and "FINDING TEXT" in kept_answer and "passed" in kept_answer, kept_answer
# A worker's detail lists the changed files of its recorded patch, only while the digest matches.
import hashlib
import panel_results
patch_dir = project / ".oms/artifacts/delegate"
patch_dir.mkdir(parents=True, exist_ok=True)
patch_bytes = b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1,2 @@\n-old\n+new\n+more\n"
(patch_dir / "fixture-diffstat.patch").write_bytes(patch_bytes)
patch_row = {"patch": ".oms/artifacts/delegate/fixture-diffstat.patch", "patch_sha256": hashlib.sha256(patch_bytes).hexdigest()}
stat = panel_results._diffstat(project, patch_row)
assert stat == {"files": [["value.txt", 2, 1]], "count": 1, "added": 2, "removed": 1}, stat
# Content lines that look like headers still count once a hunk has started.
tricky = (b"diff --git a/x b/dir b/y.txt\n--- a/dir b/y.txt\n+++ b/dir b/y.txt\n@@ -1,2 +1,2 @@\n"
          b"--- removed dashes\n+++ added pluses\n")
(patch_dir / "fixture-tricky.patch").write_bytes(tricky)
tricky_stat = panel_results._diffstat(project, {"patch": ".oms/artifacts/delegate/fixture-tricky.patch",
                                                "patch_sha256": hashlib.sha256(tricky).hexdigest()})
assert tricky_stat["files"] == [["dir b/y.txt", 1, 1]], tricky_stat
two_files = (b"diff --git a/one.txt b/one.txt\n--- a/one.txt\n+++ b/one.txt\n@@ -1 +1,2 @@\n-a\n+b\n+c\n"
             b"diff --git a/two.txt b/two.txt\n--- a/two.txt\n+++ b/two.txt\n@@ -1 +1 @@\n-x\n+y\n")
(patch_dir / "fixture-two.patch").write_bytes(two_files)
assert panel_results._diffstat(project, {"patch": ".oms/artifacts/delegate/fixture-two.patch",
    "patch_sha256": hashlib.sha256(two_files).hexdigest()})["files"] == [["one.txt", 2, 1], ["two.txt", 1, 1]]
# Git frames patch lines with LF even when file content contains other line separators.
line_repo = temporary / "diffstat-lines"
line_repo.mkdir()
subprocess.run(["git", "init", "-q", str(line_repo)], check=True)
line_content = {"formfeed.txt": "hi\x0c@@y\n", "separator.txt": "hello\u2028@@x\n"}
for name in line_content:
    (line_repo / name).write_bytes(b"")
subprocess.run(["git", "-C", str(line_repo), "add", "--", *line_content], check=True)
for name, content in line_content.items():
    (line_repo / name).write_bytes(content.encode("utf-8"))
line_patch = subprocess.run(["git", "-C", str(line_repo), "diff", "--binary", "--", *line_content],
                            capture_output=True, check=True).stdout
assert all(("+" + content).encode("utf-8") in line_patch for content in line_content.values()), line_patch
(patch_dir / "fixture-lines.patch").write_bytes(line_patch)
line_row = {"patch": ".oms/artifacts/delegate/fixture-lines.patch",
            "patch_sha256": hashlib.sha256(line_patch).hexdigest()}
assert panel_results._patch_paths(line_patch) == list(line_content)
assert panel_results._diffstat(project, line_row) == {
    "files": [[name, 1, 0] for name in line_content], "count": 2, "added": 2, "removed": 0}
invalid_hunk = line_patch.replace(b"@@ -0,0 +1 @@", b"@@ -x +1 @@", 1)
assert invalid_hunk != line_patch
(patch_dir / "fixture-invalid-hunk.patch").write_bytes(invalid_hunk)
invalid_row = {"kind": "delegate", "task_id": "invalid-hunk",
               "patch": ".oms/artifacts/delegate/fixture-invalid-hunk.patch",
               "patch_sha256": hashlib.sha256(invalid_hunk).hexdigest()}
for parse in (lambda: panel_results._patch_paths(invalid_hunk),
              lambda: panel_results._diffstat(project, invalid_row)):
    try:
        parse()
    except ValueError as error:
        assert str(error) == "patch paths unavailable: invalid hunk header", error
    else:
        raise AssertionError("invalid hunk header accepted")
with patch.object(panel_results, "_records", return_value=([], [invalid_row])):
    assert panel_results.results(project)["rows"][0]["calls"][0]["changes"] == {
        "issue": "patch evidence unavailable"}
for bad in (dict(patch_row, patch_sha256="0" * 64), dict(patch_row, patch="../outside.patch")):
    try:
        panel_results._diffstat(project, bad)
    except ValueError:
        pass
    else:
        raise AssertionError("unverified patch evidence was summarized")
changed_nav = {"tab": "detail", "preview": {"target": ("result", "builder"), "report": {"rows": [{"calls": [
    {"attempt_id": "builder", "patch": patch_row["patch"], "answer": "Patched the parser.", "changes": stat}]}]}}}
picture = render(flow, "codex", 100, 34, view="graph", main_attempt="flow-attempt", navigation=changed_nav)
changed_nav["band_offsets"]["detail"] = 99
picture = render(flow, "codex", 100, 34, view="graph", main_attempt="flow-attempt", navigation=changed_nav)
assert "Patch:" in picture and "Changes:" not in picture, picture
full_changes, _ = sys.modules["room_view"].detail(next(m for m in sys.modules["room_view"].nodes(flow) if m["participant"] == "builder"),
    changed_nav["preview"], 96)
assert "value.txt +2 -1" in "\n".join(full_changes), full_changes
# The board is on the left in landscape and above chat in portrait. Each window keeps its own record; legacy left
# percentages are converted on that window's axis, and orientation flips retain the board pane.
assert panel.parse_split("left:15c") == ("left", 15, False) and panel.parse_split("left:40") == ("left", 40, True)
assert panel.parse_split("left:3") is None and panel.parse_split("x") is None and panel.parse_split("left:0c") is None
assert panel.parse_split("top:25r") == ("top", 25, False) and panel.parse_split("top:25c") is None
assert panel.split_text(30) == "left:30c" and panel.top_text(30) == "top:30r"
assert panel.work_split_cells(("left", 248, False), 400) == 152
assert panel.work_split_cells(("left", 190, False), 200) == 76
assert panel.work_split_cells(("left", 190, False), 160) == 39
assert 160 - panel.work_split_cells(("left", 190, False), 160) - 1 >= 120
assert panel.work_split_cells(("left", 190, False), 120) == 99, "narrow windows keep the existing chat minimum"
assert panel.split_cells(("left", 40, True), 220) == 88 and panel.split_cells(("left", 25, False), 50) == 25
assert panel.split_cells(("left", 5, False), 100) == 20 and panel.split_cells(("left", 95, False), 100) == 79
assert panel.split_cells(("left", 30, False), 30) == 15
def board_calls(display, terminal=None, percent="", failed=False, main_watch=False):
    def answer(command, **kwargs):
        is_geometry = "display-message" in command
        return subprocess.CompletedProcess(command, int(failed and is_geometry), display if is_geometry else "%2" if "-P" in command else "", "")
    with patch.dict(os.environ, {"OMS_PANEL_BOARD_PERCENT": percent}), \
            patch.object(panel, "terminal_window", return_value=terminal), \
            patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        panel.board_split("%1", Path("/repo"), "board", main_watch=main_watch)
    return [call.args[0][1:] for call in tmux_run.call_args_list if "display-message" not in call.args[0]]
def left_split(width):
    return ["split-window", "-f", "-h", "-b", "-l", width, "-t", "%1", "-c", "/repo", "board"]
record_left = ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:56c"]
legacy_left = ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:76c"]
assert board_calls("200 50 1 $1\tcodex\t") == [record_left, left_split("56")]
assert board_calls("400 50 1 $1\tcodex\tleft:248c\t") == [left_split("152")]
assert board_calls("200 50 1 $1\tcodex\tleft:190c\t") == [left_split("76")]
assert board_calls("120 50 1 $1\tcodex\tleft:95c\t") == [left_split("79")]
assert board_calls("400 50 1 $1\tcodex\tleft:180c\twork\tleft:180c@400") == [left_split("180")]
assert board_calls("200 50 1 $1\tcodex\tleft:190c\toverview") == [left_split("124")]
assert board_calls("200 50 1 $1\tclaude\tleft:70c") == [left_split("70")]
assert board_calls("200 50 1 $1\tcodex\tleft:40") == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:76c"], left_split("76")]
assert board_calls("200 50 1 $1\tcodex\ttop:12c") == [record_left, left_split("56")], "an older top record was kept"
def top_split(size):
    return ["split-window", "-f", "-v", "-b", "-l", size, "-t", "%1", "-c", "/repo", "board"]
# A physically tall window (rows * 2 >= 1.1 * columns) puts the board on top even when it is wide enough for the
# left; 157x131 is the reported panel. Near-square and wide windows start left, and the hysteresis band keeps a
# recorded edge.
assert board_calls("80 90 1 $1\tcodex\t") == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "top:41r"], top_split("41")]
assert board_calls("157 131 1 $1\tcodex\t") == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "top:59r"], top_split("59")]
assert board_calls("220 50 1 $1\tcodex\t") == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:62c"], left_split("62")]
assert board_calls("100 50 1 $1\tcodex\t")[-1] == left_split("28"), "a square window starts left"
assert board_calls("80 43 1 $1\tcodex\t")[-1] == left_split("22"), "below the enter threshold stays left"
assert board_calls("80 44 1 $1\tcodex\t")[-1] == top_split("20"), "the enter threshold is portrait"
assert board_calls("80 40 1 $1\tcodex\t")[-1] == left_split("22"), "a tall window without 41 rows cannot split on top"
assert board_calls("100 50 1 $1\tclaude\ttop:23r")[-1] == top_split("23"), "a recorded top edge survives the band"
assert board_calls("100 50 1 $1\tclaude\tleft:30c")[-1] == left_split("30")
assert board_calls("200 50 1 $1\tclaude\ttop:23r")[-1] == left_split("56"), "a wide window leaves a recorded top edge"
assert board_calls("157 131 1 $1\tclaude\tleft:40c")[-1] == top_split("59"), "a tall window leaves a recorded left edge"
assert board_calls("80 24 0 $1\tcodex\t", terminal=(157, 131)) == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "top:59r"], top_split("45%")], "the attaching terminal is sized too"
assert board_calls("200 50 1 $1\tclaude\tleft:70c") == [left_split("70")]
assert board_calls("200 50 1 $1\tcontrol\t") == [left_split("56")]
assert board_calls("") == [left_split("28%")]
assert board_calls("broken geometry", failed=True) == [left_split("28%")]
# A detached session still reports tmux's 80x24: the attaching terminal sizes the board and the record.
assert board_calls("80 24 0 $1\tcodex\t", terminal=(240, 59)) == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:67c"], left_split("28%")]
assert board_calls("80 24 0 $1\tcodex\t") == [left_split("28%")]
for configured, cells in (("25", 50), ("35", 70), ("1", 50), ("99", 70), ("-1", 50),
                          ("garbage", 56), ("28.5", 56), ("2_8", 56)):
    assert board_calls("200 50 1 $1\tcodex\t", percent=configured) == [
        ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:%dc" % cells], left_split(str(cells))]
    assert board_calls("200 50 1 $1\tclaude\tleft:90c", percent=configured) == [left_split("76")]
assert board_calls("50 24 1 $1\tcodex\t", percent="25") == []
# New boards require 20 tree cells and 40 native CLI cells, including the border in the budget.
split_widths = ((42, None), (60, None), (63, 20), (64, 20), (80, 22), (127, 36))
for window, cells in split_widths:
    assert panel.board_split_cells(("left", panel.BOARD_SHARE, True), window) == cells
    if cells is not None:
        assert cells == panel.split_cells(("left", panel.BOARD_SHARE, True), window)
        assert cells >= 20 and window - cells - 1 >= 40
assert panel.board_split_cells(("left", 28, True), 61) == 20
assert panel.board_split_cells(("left", 35, True), 61) == 20  # the default share yields to the CLI minimum
assert panel.board_split_cells(("left", 70, False), 80) == 39
# A dragged width is kept once the window can hold both minimums.
assert panel.board_split_cells(("left", 70, False), 110, explicit=True) == 70
assert panel.board_split_cells(("left", 70, False), 60, explicit=True) is None
assert board_calls("200 50 1 $1\tcodex\t") == [record_left, left_split("56")]
assert board_calls("200 50 1 $1\tclaude\tleft:70c") == [left_split("70")]
assert board_calls("200 50 1 $1\tcodex\tleft:40") == [legacy_left, left_split("76")]
assert board_calls("200 50 1 $1\tcodex\ttop:12c") == [record_left, left_split("56")], "an older top record was kept"
assert board_calls("200 50 1 $1\tcontrol\t") == [left_split("56")]
assert board_calls("200 50 1 $1\tclaude\t", main_watch=True) == [
    record_left,
    ["split-window", "-P", "-F", "#{pane_id}", "-f", "-h", "-b", "-l", "56", "-t", "%1", "-c", "/repo", "board"],
    ["set-option", "-p", "-t", "%2", "@oms_panel_main_watch", "1"]]
assert board_calls("80 90 1 $1\tclaude\t", main_watch=True) == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "top:41r"],
    ["split-window", "-P", "-F", "#{pane_id}", "-f", "-v", "-b", "-l", "41", "-t", "%1", "-c", "/repo", "board"],
    ["set-option", "-p", "-t", "%2", "@oms_panel_main_watch", "1"]]
control_input = {"pane": "%10", "window": "@1", "session": "oms-fixture", "watch": False}
for identity, expected_watch in (({}, False),
                                 ({"@oms_panel_main_attempt": "main-attempt",
                                   "@oms_panel_room_participant": "main-member",
                                   "@oms_panel_repo": "/repo"}, True)):
    with patch.object(panel, "panel_panes", side_effect=[[control_input], [control_input]]), \
            patch.object(panel, "pane_option", side_effect=lambda key: identity.get(key, "")), \
            patch.object(panel, "board_split") as split, \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "graph", "")):
        panel.reopen_panel(Path("/repo"))
    assert split.call_args.kwargs["main_watch"] is expected_watch, (identity, split.call_args)
# A detached session still reports tmux's 80x24: the attaching terminal sizes the board and the record.
assert board_calls("80 24 0 $1\tcodex\t", terminal=(240, 59)) == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:67c"], left_split("28%")]
assert board_calls("80 24 0 $1\tcodex\t") == [left_split("28%")]
for window, cells in split_widths:
    for height in (24, 40):
        orientation = "left" if cells is not None else "top"
        extent = cells if cells is not None else panel.board_split_cells(("top", panel.BOARD_HEIGHT_SHARE, True), height)
        axis = window if orientation == "left" else height
        value = (panel.split_text(extent) if orientation == "left" else panel.top_text(extent)) if extent is not None else ""
        record = [] if extent is None else [["set-option", "-w", "-t", "%1", "@oms_panel_split", value]]
        def expected_split(size):
            return ["split-window", "-f", "-h" if orientation == "left" else "-v", "-b", "-l", size,
                    "-t", "%1", "-c", "/repo", "board"]
        attached_split = [] if extent is None else [expected_split(str(extent))]
        detached_split = [] if extent is None else [expected_split("%d%%" % ((200 * extent + axis) // (2 * axis)))]
        for name in ("control", "claude"):
            recorded = record if name == "claude" else []
            assert board_calls("%d %d 1 $1\t%s\t" % (window, height, name)) == recorded + attached_split
            assert board_calls("80 24 0 $1\t%s\t" % name, terminal=(window, height)) == recorded + detached_split
assert board_calls("60 24 0 $1\tclaude\t") == [], "unknown terminal size ignored a narrow tmux window"
# A saved width keeps its preference while the applied board leaves the native CLI's 40-cell floor.
assert board_calls("80 24 1 $1\tclaude\tleft:70c") == [left_split("39")], \
    "a saved width leaves 40 native CLI columns without rewriting the saved option"
assert board_calls("200 50 1 $1\tclaude\tleft:70c") == [left_split("70")]
for configured, cells in (("25", 50), ("35", 70), ("1", 50), ("99", 70), ("-1", 50),
                          ("garbage", 56), ("28.5", 56), ("2_8", 56)):
    assert board_calls("200 50 1 $1\tcodex\t", percent=configured) == [
        ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:%dc" % cells], left_split(str(cells))]
    assert board_calls("200 50 1 $1\tclaude\tleft:90c", percent=configured) == [left_split("76")]
assert board_calls("50 24 1 $1\tcodex\t", percent="25") == []
assert board_calls("61 24 1 $1\tcodex\t", percent="35") == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:20c"], left_split("20")]
# Unknown terminal size: the share is taken from the clamped cells, so the CLI keeps its 40 cells.
assert board_calls("61 24 0 $1\tcodex\t", percent="35") == [left_split("33%")]
def sync_run(sync, *reads):
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run") as tmux_run:
        done = [sync.tick(read) for read in reads]
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
def sync_run_panes(sync, reads, listing):
    def answer(command, **kwargs):
        return subprocess.CompletedProcess(command, 0, listing if command[1] == "list-panes" else "", "")
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        done = [sync.tick(read) for read in reads]
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
def reading(active, window, pane, text="", zoomed=False, panes=2, mode="work", native="%2", drag=""):
    return (active, zoomed, panes, window, pane, text, mode, native, drag)
# Applying a saved landscape width keeps the native CLI at 40 columns without rewriting the preference.
done, calls = sync_run(panel.SplitSync(), reading(False, (80, 40), (59, 40), "left:70c"))
assert done == ["follow"] and calls == [["resize-pane", "-t", "%1", "-x", "39"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (70, 50), "left:70c"))
assert done == [None] and calls == [], (done, calls)
# A border drag in the active window is recorded as cells (clamped) and an inactive window resizes to the record.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (100, 50), (22, 50), "left:25c"))
assert done == [None, "record"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:22c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:22@100"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(True, (400, 50), (152, 50), "left:152c"),
                       reading(True, (400, 50), (180, 50), "left:152c"),
                       reading(True, (400, 50), (180, 50), "left:180c", drag="left:180@400"))
assert done == [None, "record", None] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:180c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:180@400"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (400, 50), (180, 50), "left:180c",
                                                 drag="left:180@400"))
assert done == [None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (317, 50), (196, 50), "left:196c"))
assert done == ["follow"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:120c"],
    ["resize-pane", "-t", "%1", "-x", "120"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (317, 50), (196, 50), "left:196c",
                                                 drag="left:196@317"))
assert done == [None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (400, 50), (180, 50), "left:180c",
                                                 drag="left:180@400"),
                       reading(False, (200, 50), (90, 50), "left:180c", drag="left:180@400"))
assert done == [None, None] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:90c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:90@200"]], (done, calls)
for mode in ("overview", "focus"):
    done, calls = sync_run(panel.SplitSync(), reading(True, (317, 50), (196, 50), "left:196c",
                                                    mode=mode, drag="left:196@317"))
    assert done == [None] and calls == [], (mode, done, calls)
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (100, 50), (5, 50), "left:25c"))
assert done == [None, "record"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:20c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:20@100"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (25, 50), "left:22c"), reading(False, (100, 50), (22, 50), "left:22c"))
assert done == ["follow", None] and calls == [["resize-pane", "-t", "%1", "-x", "22"]], (done, calls)
# A reader pane splitting the chat column (three panes) leaves the board synced.
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (130, 50), "left:80c", panes=3))
assert done == ["follow"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:76c"],
    ["resize-pane", "-t", "%1", "-x", "76"]], (done, calls)
# Work caps a stored share, Overview uses a temporary graph width, and Focus only zooms the native CLI pane.
done, calls = sync_run(panel.SplitSync(), reading(False, (400, 50), (250, 50), "left:250c"))
assert done == ["follow"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:152c"],
    ["resize-pane", "-t", "%1", "-x", "152"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (56, 50), "left:56c", mode="overview"))
assert done == [None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (56, 50), "left:56c", mode="focus"))
assert done == [None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (56, 50), "left:56c", zoomed=True,
                                                 mode="overview"))
assert done == [None] and calls == [], (done, calls)
# Cycling updates only the window mode. It leaves the per-window Work geometry untouched.
mode = [""]
def cycle_answer(command, **kwargs):
    if command[1] == "display-message":
        return subprocess.CompletedProcess(command, 0, mode[0], "")
    if command[1] == "set-option":
        mode[0] = command[-1]
    return subprocess.CompletedProcess(command, 0, "", "")
with patch.object(panel.subprocess, "run", side_effect=cycle_answer) as mode_run:
    cycled = [panel.cycle_panel_mode("%1") for _ in range(3)]
assert cycled == ["overview", "focus", "work"] and mode[0] == "work", cycled
assert [call.args[0][1:] for call in mode_run.call_args_list if call.args[0][1] == "set-option"] == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_mode", name] for name in cycled]
assert "F8" in dict((key, command) for key, command in panel.MAIN_KEYS)
assert all(hint in panel.keys_help(())[1] for hint in ("F8 Modes", "i Headers"))
root_calls = []
def root_key_run(argv, **kwargs):
    if argv[1] == "list-keys" and argv[-1] == "F8":
        return subprocess.CompletedProcess(argv, 0, "bind-key -n F8 display-message user\n", "")
    root_calls.append(argv[1:])
    return subprocess.CompletedProcess(argv, 1 if argv[1] == "list-keys" else 0, "", "")
with patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
        patch.object(panel.subprocess, "run", side_effect=root_key_run):
    panel.bind_main_keys()
assert not any(call[0] == "bind-key" and call[2] == "F8" for call in root_calls), root_calls
sequence = [reading(False, (200, 50), (56, 50), "left:56c"),
            reading(False, (200, 50), (56, 50), "left:56c", mode="overview"),
            reading(False, (200, 50), (124, 50), "left:56c", mode="focus"),
            reading(False, (200, 50), (124, 50), "left:56c", zoomed=True, mode="focus"),
            reading(False, (200, 50), (124, 50), "left:56c", zoomed=True),
            reading(False, (200, 50), (124, 50), "left:56c")]
def mode_sequence_run(reads):
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run") as tmux_run:
        modes, splits = panel.PanelModeSync(), panel.SplitSync()
        done = []
        for read in reads:
            split = splits.tick(read)
            done.append((modes.tick(read), split))
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
done, calls = mode_sequence_run(sequence)
assert [split for unused, split in done] == [None, None, None, None, None, "follow"], (done, calls)
assert calls == [["resize-pane", "-t", "%1", "-x", "124"],
                 ["resize-pane", "-Z", "-t", "%2"],
                 ["resize-pane", "-Z", "-t", "%2"],
                 ["resize-pane", "-t", "%1", "-x", "56"]], calls
assert not any(call[0] == "set-option" for call in calls), calls
# Overview and Focus resizes do not replace the last Work proportion.
done, calls = mode_sequence_run([
    reading(False, (400, 50), (112, 50), "left:112c"),
    reading(False, (400, 50), (56, 50), "left:112c", mode="overview"),
    reading(False, (200, 50), (56, 50), "left:112c", mode="overview"),
    reading(False, (200, 50), (56, 50), "left:112c", zoomed=True, mode="focus"),
    reading(False, (200, 50), (76, 50), "left:112c", zoomed=True),
    reading(False, (200, 50), (76, 50), "left:112c")])
assert [split for unused, split in done] == [None, None, None, None, None, "follow"], (done, calls)
assert calls[-2:] == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:56c"],
    ["resize-pane", "-t", "%1", "-x", "56"]], calls
# A marked Work drag keeps its proportion when the window shrinks in Overview.
done, calls = mode_sequence_run([
    reading(False, (400, 50), (180, 50), "left:180c", drag="left:180@400"),
    reading(False, (400, 50), (248, 50), "left:180c", mode="overview", drag="left:180@400"),
    reading(False, (200, 50), (124, 50), "left:180c", mode="overview", drag="left:180@400"),
    reading(False, (200, 50), (124, 50), "left:180c", drag="left:180@400")])
assert done[-1][1] == "follow" and calls[-1] == ["resize-pane", "-t", "%1", "-x", "90"], (done, calls)
assert [call for call in calls if call[0] == "set-option"] == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:90c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:90@200"]], calls
# A portrait board flips during Overview; Overview sizing waits until the board is beside the chat pane.
def mode_flip_run(reads, listing):
    def answer(command, **kwargs):
        return subprocess.CompletedProcess(command, 0, listing if command[1] == "list-panes" else "", "")
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        modes, splits = panel.PanelModeSync(), panel.SplitSync()
        done = []
        for read in reads:
            split = splits.tick(read)
            done.append((modes.tick(read), split))
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
done, calls = mode_flip_run([
    reading(False, (60, 90), (60, 40), "top:40r"),
    reading(False, (200, 50), (200, 25), "top:40r", mode="overview"),
    reading(False, (200, 50), (89, 50), "top:40r", mode="overview")],
    "%1 200 25\n%2 80 25\n")
assert [split for unused, split in done] == [None, "follow", None], (done, calls)
assert calls == [["list-panes", "-t", "%1", "-F", "#{pane_id} #{pane_width} #{pane_height}"],
                 ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-h", "-l", "89"],
                 ["resize-pane", "-t", "%1", "-x", "124"]], calls
assert not any(call[0] == "set-option" for call in calls), calls
# Returning after Overview changed the edge restores the Work fraction on the new axis.
done, calls = mode_flip_run([
    reading(False, (60, 90), (60, 40), "top:40r"),
    reading(False, (200, 50), (200, 25), "top:40r", mode="overview"),
    reading(False, (200, 50), (124, 50), "top:40r", mode="overview"),
    reading(False, (200, 50), (124, 50), "top:40r")],
    "%1 200 25\n%2 80 25\n")
assert done[-1][1] == "follow" and calls[-2:] == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:76c"],
    ["resize-pane", "-t", "%1", "-x", "76"]], (done, calls)
# Entering Focus during an edge flip moves the board before zooming the CLI.
done, calls = mode_flip_run([
    reading(False, (200, 50), (56, 50), "left:56c"),
    reading(False, (60, 90), (30, 90), "left:56c", mode="focus")],
    "%1 30 90\n%2 49 90\n")
assert done[-1] == ("focus", "follow") and calls[-2:] == [
    ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-v", "-l", "25"],
    ["resize-pane", "-Z", "-t", "%2"]], (done, calls)
# Overview and Focus also flip landscape boards back to portrait without changing Work geometry.
for mode in ("overview", "focus"):
    done, calls = mode_flip_run([
        reading(False, (200, 50), (124, 50), "left:124c", mode=mode),
        reading(False, (60, 90), (40, 90), "left:124c", mode=mode)],
        "%1 40 90\n%2 40 45\n")
    assert [split for unused, split in done] == [None, "follow"], (mode, done, calls)
    assert [call for call in calls if call[0] == "join-pane"] == [
        ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-v", "-l", "56"]], (mode, calls)
    assert not any(call[0] == "set-option" for call in calls), (mode, calls)
# The active window enforces the record too, and its own resize is not read back as a drag.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (30, 50), "left:22c"), reading(True, (100, 50), (21, 50), "left:22c"))
assert done == ["follow", None] and calls == [["resize-pane", "-t", "%1", "-x", "22"]], (done, calls)
# A same-orientation window resize scales the recorded board share proportionally.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50), "left:25c"), reading(True, (120, 50), (30, 50), "left:25c"))
assert done == [None, "record"] and calls == [["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:30c"]], (done, calls)
# When a resize leaves the pane on a new edge, rescale its prior fraction and record that edge; never use left columns as top rows.
done, calls = sync_run(panel.SplitSync(), reading(False, (80, 40), (22, 40), "left:22c"),
                       reading(False, (60, 40), (60, 25), "left:22c"),
                       reading(False, (65, 40), (65, 25), "top:25r"))
assert done == [None, "follow", "record"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "top:20r"],
    ["resize-pane", "-t", "%1", "-y", "20"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:41c"],
    ["list-panes", "-t", "%1", "-F", "#{pane_id} #{pane_width} #{pane_height}"]], (done, calls)
# Cap after scaling the original record, so repeated window shrinks preserve its fraction.
done, calls = sync_run(panel.SplitSync(), reading(True, (400, 50), (112, 50), "left:112c"),
                       reading(True, (200, 50), (56, 50), "left:112c"),
                       reading(True, (100, 50), (28, 50), "left:56c"))
assert done == [None, "record", "record"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:56c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:28c"]], (done, calls)
# A one-cell gap may still violate the chat reserve, so it must trigger a resize.
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (76, 50), "left:76c"),
                       reading(False, (160, 50), (40, 50), "left:76c"))
assert done == [None, "follow"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:39c"],
    ["resize-pane", "-t", "%1", "-x", "39"]], (done, calls)
# Separate watchers write only their own window option, even though both windows share a tmux session.
with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s"}), patch.object(panel.subprocess, "run") as tmux_run:
    for pane_id in ("%1", "%9"):
        sync = panel.SplitSync()
        with patch.dict(os.environ, {"TMUX_PANE": pane_id}):
            sync.tick(reading(True, (100, 50), (25, 50), "left:25c"))
            assert sync.tick(reading(True, (100, 50), (22, 50), "left:25c")) == "record"
window_records = [call.args[0][1:] for call in tmux_run.call_args_list if call.args[0][1] == "set-option"]
assert window_records == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:22c"],
    ["set-option", "-w", "-t", "%1", "@oms_panel_work_drag", "left:22@100"],
    ["set-option", "-w", "-t", "%9", "@oms_panel_split", "left:22c"],
    ["set-option", "-w", "-t", "%9", "@oms_panel_work_drag", "left:22@100"]], window_records
# Windows without a record are left alone; the older percent form is converted only where recorded.
done, calls = sync_run(panel.SplitSync(), reading(True, (100, 50), (25, 50)), reading(False, (100, 50), (25, 50)))
assert done == [None, None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (200, 50), (60, 50), "left:30"))
assert done == ["record"] and calls == [["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:60c"]], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (25, 50), "left:30"))
assert done == ["follow"] and calls == [
    ["set-option", "-w", "-t", "%1", "@oms_panel_split", "left:30c"],
    ["resize-pane", "-t", "%1", "-x", "30"]], (done, calls)
# Zoom remains untouched, while the non-zoomed top board in a landscape window flips to a left split.
def zoom_and_flip(first, second):
    def answer(command, **kwargs):
        out = "%1 200 25\n%2 100 50\n" if command[1] == "list-panes" else ""
        return subprocess.CompletedProcess(command, 0, out, "")
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        sync = panel.SplitSync()
        done = [sync.tick(first), sync.tick(second)]
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
done, calls = zoom_and_flip(reading(False, (100, 50), (100, 50), "left:30c", zoomed=True),
                            reading(False, (200, 50), (200, 25), "left:15c"))
assert done == [None, "follow"] and calls[-1] == [
    "join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-h", "-l", "20"], (done, calls)
# Orientation changes move the existing board pane to the new edge and keep other pane ids in place.
def flip_run(first, second, listing):
    def answer(command, **kwargs):
        if command[1] == "list-panes":
            return subprocess.CompletedProcess(command, 0, listing, "")
        return subprocess.CompletedProcess(command, 0, "", "")
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "s", "TMUX_PANE": "%1"}), \
            patch.object(panel.subprocess, "run", side_effect=answer) as tmux_run:
        sync = panel.SplitSync()
        sync.tick(first)
        done = [sync.tick(second)]
    return done, [call.args[0][1:] for call in tmux_run.call_args_list]
done, calls = flip_run(reading(False, (120, 50), (30, 50), "left:30c"),
                       reading(False, (60, 90), (30, 90), "left:30c"),
                       "%1 60 30\n%2 30 45\n%3 30 44\n")
assert done[-1] == "follow" and [call for call in calls if call[0] == "join-pane"] == [
    ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-v", "-l", "23"]], (done, calls)
done, calls = flip_run(reading(False, (200, 50), (60, 50), "left:30"),
                       reading(False, (60, 90), (24, 90), "left:30"),
                       "%1 80 30\n%2 40 45\n")
assert done[-1] == "follow" and [call for call in calls if call[0] == "join-pane"] == [
    ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-v", "-l", "27"]], (done, calls)
done, calls = flip_run(reading(False, (60, 90), (60, 40), "top:40r"),
                       reading(False, (120, 50), (120, 40), "top:40r"),
                       "%1 120 40\n%2 60 50\n")
assert done[-1] == "follow" and [call for call in calls if call[0] == "join-pane"] == [
    ["join-pane", "-d", "-f", "-s", "%1", "-t", "%2", "-b", "-h", "-l", "53"]], (done, calls)
assert all(call[call.index("-t") + 1] in ("%1", "%2") for call in calls
           if call[0] in ("list-panes", "join-pane")), calls
# Four-cell hysteresis keeps the current orientation at 100x50 and throughout the boundary band.
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 50), (25, 50), "left:25c"),
                       reading(False, (100, 51), (25, 51), "left:25c"),
                       reading(False, (100, 52), (25, 52), "left:25c"))
assert done == [None, None, None] and calls == [], (done, calls)
# A top board returns left as soon as the width clears the left threshold plus four columns,
# and stays above the CLI inside that band (100x45 is the aspect release point).
done, calls = sync_run_panes(panel.SplitSync(), [reading(False, (100, 45), (100, 25), "top:25r")],
                             "%1 100 25\n%2 80 25\n")
assert done == ["follow"] and any(call[0] == "join-pane" and "-h" in call for call in calls), (done, calls)
done, calls = sync_run_panes(panel.SplitSync(), [reading(False, (63, 50), (63, 25), "top:25r")],
                             "%1 63 25\n%2 63 24\n")
assert done == [None] and not any(call[0] == "join-pane" for call in calls), (done, calls)
# A tall window moves a left board above the CLI (157x131 is the reported panel); a wide one does not. The band
# (rows * 2 between 0.9 and 1.1 columns) keeps either edge, and a top board returns left only at rows * 2 <= 0.9 columns.
done, calls = sync_run_panes(panel.SplitSync(), [reading(False, (157, 131), (40, 131), "left:40c")],
                             "%1 40 131\n%2 116 131\n")
assert done == ["follow"] and any(call[0] == "join-pane" and "-v" in call for call in calls), (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (220, 50), (62, 50), "left:62c"))
assert done == [None] and calls == [], (done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (100, 54), (28, 54), "left:28c"),
                       reading(False, (100, 55), (28, 55), "left:28c"))
assert done[0] is None and done[1] and calls[0][-1].startswith("top:") and calls[1][0] == "list-panes", (done, calls)
for height in (46, 50, 54):
    done, calls = sync_run_panes(panel.SplitSync(), [reading(False, (100, height), (100, 25), "top:25r")],
                                 "%1 100 25\n%2 28 25\n")
    assert done == [None] and not any(call[0] == "join-pane" for call in calls), (height, done, calls)
done, calls = sync_run(panel.SplitSync(), reading(False, (80, 43), (22, 43), "left:22c"))
assert done == [None] and calls == [], "a window without 41 rows never aspect-flips"
done, calls = sync_run_panes(panel.SplitSync(), [reading(False, (60, 30), (22, 30), "left:22c")], "%1 22 30\n%2 37 30\n")
assert done == ["follow"] and any(call[0] == "join-pane" and "-v" in call for call in calls), "narrow still means top"
# The new unsplit narrow-window path keeps the original single-pane/F9 regressions.
done, calls = sync_run(panel.SplitSync(), reading(True, (42, 24), (42, 24), "left:22c", panes=1))
assert done == [None] and calls == [], (done, calls)
assert dict(panel.MAIN_KEYS)["F9"] == "select-pane -t :.+"
# A call opens in a reader pane right of the board: the window's recorded native chat pane is split with the reader
# on top and the reader is marked with its board; a second call respawns that reader, close kills only it, and a
# short chat or a missing/foreign chat record keeps the detail in the board. Recorded identifiers are revalidated
# and every argument of the pane command is quoted.
import panel_reader
from datetime import datetime
import tempfile
import re
assert panel._review_verdict("GATE: pass\n> GATE: fail\n```text\nGATE: fail\n```\nGATE: fail",
                             with_conflict=True) == ("fail", True)
assert panel._review_verdict("> GATE: pass\n~~~\nGATE: fail\n~~~", with_conflict=True) == (None, False)
write_call = {"kind": "delegate", "attempt_id": "attempt-write", "patch": ".oms/artifacts/patch.diff"}
read_call = {"kind": "ask", "attempt_id": "attempt-read"}
timeline = [{"ts": "2026-10-12T09:02:00+09:00", "event_type": "attempt.heartbeat"},
            {"ts": "2026-10-12T09:00:00+09:00", "state": "started"},
            {"ts": "2026-10-12T09:03:00+09:00", "state": "verify"},
            {"ts": "2026-10-12T09:02:30+09:00", "event_type": "attempt.heartbeat"}]
event_view = panel_reader.render_view("Events", {}, write_call, timeline)
local_start = datetime.fromisoformat("2026-10-12T09:00:00+09:00").astimezone().strftime("%H:%M:%S")
local_verify = datetime.fromisoformat("2026-10-12T09:03:00+09:00").astimezone().strftime("%H:%M:%S")
assert event_view.index(local_start) < event_view.index(local_verify) and "2 heartbeats" in event_view, event_view
artifact_view = panel_reader.render_view("Events", {}, write_call, [
    {"ts": "2026-10-12T09:05:00+09:00", "kind": "review", "verdict": "GATE: pass", "summary": "Reviewed changes"},
    {"ts": "2026-10-12T09:06:00+09:00", "kind": "patch-land", "summary": "Landed"}])
assert "review · Reviewed changes" in artifact_view and "[GATE: pass]" in artifact_view and "patch-land · Landed" in artifact_view, artifact_view
assert any(line.endswith("[GATE: pass]") and display_width(line) == 100
           for line in artifact_view.splitlines()), artifact_view
long_events = panel_reader.render_view("Events", {}, write_call, [
    {"ts": "2026-10-12T09:05:00+09:00", "kind": "review", "verdict": "passed",
     "summary": "A long review summary that must wrap to the available terminal width without overflowing a row " * 3}], width=40)
plain_events = [re.sub(r"\033\[[0-9;]*m", "", line) for line in long_events.splitlines()]
assert all(len(line) <= 40 for line in plain_events), plain_events
truncated_events = panel_reader.render_view("Events", {}, write_call, timeline_truncated=True, width=40)
plain_truncated = [re.sub(r"\033\[[0-9;]*m", "", line) for line in truncated_events.splitlines()]
assert all(len(line) <= 40 for line in plain_truncated), plain_truncated
patch_text = b"diff --git a/file.py b/file.py\n--- a/file.py\n+++ b/file.py\n@@ -1 +1 @@\n-old\n+new\n"
diff_view = panel_reader.render_view("Diff", {}, write_call, patch=patch_text)
assert "file.py" in diff_view and "old" in diff_view and "new" in diff_view, diff_view
assert "?[32m" not in diff_view and "\033[" not in diff_view, diff_view
with patch.dict(os.environ, {"NO_COLOR": "1"}):
    plain_diff = panel_reader.render_view("Diff", {}, write_call, patch=patch_text, color=True)
assert "+new" in plain_diff and "-old" in plain_diff and "\033[" not in plain_diff, plain_diff
with patch.dict(os.environ, {"TERM": "xterm-256color"}, clear=True):
    colored_diff = panel_reader.render_view("Diff", {}, write_call, patch=patch_text, color=True)
assert "\033[38;5;114m+new\033[0m" in colored_diff and "\033[38;5;203m-old\033[0m" in colored_diff, colored_diff
indented_patch = b"diff --git a/file.py b/file.py\n@@ -1 +1 @@\n context\n+  indented\n"
assert "+  indented" in panel_reader.render_view("Diff", {}, write_call, patch=indented_patch), "diff indentation was collapsed"
assert "No patch recorded" in panel_reader.render_view("Diff", {}, read_call), "read-only calls need an explicit no-patch view"
assert "patch unreadable (digest mismatch / read error)" in panel_reader.render_view(
    "Diff", {}, write_call, patch_error=True), "invalid patch evidence was mistaken for a read-only call"
assert "patch capped" in panel_reader.render_view("Diff", {}, write_call, patch=b"x" * (panel_reader.PATCH_LIMIT + 1)), "large patches need a cap note"
assert panel_reader.VIEWS == ("Summary", "Raw", "Events", "Diff", "Run info")
assert "Summary" in panel_reader.render_view("Summary", {"rows": []}, write_call)
starting_reader = panel_reader.render_view("Summary", {"rows": []}, dict(write_call, state="working"))
assert "[⟳ running]" in starting_reader and "status unknown" not in starting_reader, starting_reader
verifier_reader = panel_reader.render_view("Summary", {"rows": [{"role": "reviewer", "purpose": "verifier",
    "model": "gpt-6.1-sol"}]}, write_call)
assert "VERIFIER Sol 6.1" in verifier_reader and "REVIEWER Sol 6.1" not in verifier_reader, verifier_reader
markdown_call = dict(write_call, answer="# Heading\n- first\n```\n  code\n```\n| a | b |\n|---|---|",
                     artifact="answer.txt", answer_truncated=True)
raw_view = panel_reader.render_view("Raw", {"rows": [{"role": "worker", "model": "gpt-6.1-sol"}]}, markdown_call)
assert "# Heading" in raw_view and "  - first" in raw_view and "  code" in raw_view and "| a | b |" in raw_view
assert "partial answer" in raw_view and "\033[7m" not in raw_view, raw_view
run_info = panel_reader.render_view("Run info", {"rows": [{"provider": "codex", "model": "gpt-6.1-sol",
    "process_state": "failed", "issue": "provider error"}]}, markdown_call)
assert "Provider: codex" in run_info and "Attempt: attempt-write" in run_info and "answer.txt" in run_info, run_info
assert "Ending: failed" in run_info and "Error: provider error" in run_info, run_info
# Attention's result action must leave a useful reader even when the failure has no answer artifact.
for ending, error, location, expected in (
        ("guard_stop", "guard: workspace boundary", "/tmp/kept-tree",
         ("guard stop", "workspace boundary", "oms panel --salvage failed-task")),
        ("cancelled", "provider cancelled", None, ("cancelled", "provider cancelled")),
        ("timed_out", None, None, ("timed out", "retry"))):
    failed_report = {"rows": [{"task_id": "failed-task", "role": "worker", "process_state": ending,
                               "issue": error, "location": location, "next_action": "retry failed stage", "calls": []}]}
    failed_view = panel_reader.render_view("Summary", failed_report, {"attempt_id": "failed-attempt"}, color=False)
    assert all(part in failed_view for part in expected), failed_view
    assert "No result recorded yet" in failed_view and "Summary" in failed_view, failed_view
artifact_failure = panel_reader.render_view("Summary", {"rows": [{"task_id": "failed-task", "role": "worker",
    "process_state": "failed", "calls": [{"attempt_id": "artifact-attempt", "state": "failed",
    "artifact": "failure.txt", "answer": "Recorded failure details"}]}]},
    {"attempt_id": "artifact-attempt", "state": "failed", "artifact": "failure.txt", "answer": "Recorded failure details"}, color=False)
assert "Recorded failure details" in artifact_failure and "Ending:" not in artifact_failure, artifact_failure
assert panel_reader.TerminalInput(extra_keys=("r", "e", "d")).decode(b"\tred") == [("tab",), ("r",), ("e",), ("d",)]
assert panel_reader.TerminalInput().decode(b"red") == [("unbound",)] * 3
with tempfile.TemporaryDirectory() as reader_repo_name:
    reader_repo = Path(reader_repo_name)
    lifecycle = reader_repo / ".oms/lifecycle"
    lifecycle.mkdir(parents=True)
    reader_ledger = lifecycle / "events.jsonl"
    reader_ledger.write_text("".join(json.dumps({"attempt_id": "old", "event_type": "attempt.created", "ts": "old"}) + "\n"
                             for unused in range(20)) +
                      json.dumps({"attempt_id": "reader-attempt", "event_type": "attempt.created", "ts": "now"}) + "\n" +
                      json.dumps({"attempt_id": "reader-attempt", "event_type": "attempt.heartbeat", "ts": "later"}) + "\n",
                      encoding="utf-8")
    recent, truncated = panel_reader.event_tail(reader_repo, "reader-attempt", maximum=1024)
    assert [item["event_type"] for item in recent] == ["attempt.created", "attempt.heartbeat"] and not truncated, recent
    recent, truncated = panel_reader.event_tail(reader_repo, "reader-attempt", maximum=90)
    assert truncated and not any(item["event_type"] == "attempt.created" for item in recent), (recent, truncated)
assert panel_reader.target_args(("result", "worker-1")) == ["--results", "--participant", "worker-1"]
assert panel_reader.target_args(("debate", ("task.1", "seat-a"))) == [
    "--results", "--participant", "seat-a", "--task-id", "task.1"]
assert panel_reader.target_args(("mail", "main-1")) == ["--mail", "main-1"]
for bad in (("result", "-x"), ("result", "a b"), ("result", "$(touch x)"), ("result", None),
            ("debate", ("-t", "seat")), ("debate", ("t;x", "seat")), ("chat", "main-1"),
            ("mail", "-x"), ("mail", "a b"), ("mail", "$(touch x)"), ("mail", None)):
    try:
        panel_reader.target_args(bad)
    except ValueError:
        pass
    else:
        raise AssertionError(bad)
production_call = {"kind": "review", "attempt_id": "call-attempt", "event_id": "review-event",
                   "patch": ".oms/artifacts/review.patch", "patch_sha256": "digest", "exit": 0,
                   "answer": "Reviewed the change"}
production_report = {"rows": [{"task_id": "reader-task", "role": "worker", "model": "fixture",
                                "process_state": "settled", "calls": [production_call]}]}
refresh_cache = {}
running_report = {"rows": [{"role": "worker", "model": "fixture", "process_state": "working",
                            "calls": [production_call]}]}
finished_report = {"rows": [{"role": "worker", "model": "fixture", "process_state": "done",
                             "calls": [production_call]}]}
with patch.object(panel, "results_snapshot", side_effect=[(running_report, ([], [])),
                                                            (finished_report, ([], []))]) as collected:
    running_view = panel.render_call_view(project, "worker-1", "Summary", 100, cache=refresh_cache)
    refresh_cache.clear()
    finished_view = panel.render_call_view(project, "worker-1", "Summary", 100, cache=refresh_cache)
assert collected.call_count == 2 and "running" in running_view and "finished" in finished_view, (
    collected.call_count, running_view, finished_view)
artifactless_attempt = {"attempt_id": "guarded-attempt", "task_id": "guarded-task", "state": "failed",
    "reason_code": "worker_authority_violation", "refs": {"panel_room_participant": "guarded-worker",
    "panel_role": "worker", "panel_label": "Guarded work", "panel_location": "worktree:kept"}}
artifactless_report = {"rows": [{"task_id": "guarded-task", "role": "worker", "calls": []}]}
with patch.object(panel, "results_snapshot", return_value=(artifactless_report, ([artifactless_attempt], []))):
    artifactless_view = panel.render_call_view(project, "guarded-worker", "Summary", 100)
    artifactless_info = panel.render_call_view(project, "guarded-worker", "Run info", 100)
assert all(part in artifactless_view for part in ("guard stop", "worker_authority_violation",
    "worktree:kept", "oms panel --salvage guarded-task")), artifactless_view
assert all(part in artifactless_info for part in ("Ending: guard stop", "worker_authority_violation",
    "Kept worktree: worktree:kept")), artifactless_info
indexed_events = [
    {"kind": "review", "attempt_id": "call-attempt", "event_id": "review-event", "ts": "2026-10-12T09:03:00Z"},
    {"kind": "patch-land", "task_id": "reader-task", "patch_sha256": "digest",
     "ts": "2026-10-12T09:04:00Z", "summary": "Landed"},
    {"kind": "panel-delivery", "task_id": "reader-task", "artifact": "delivery.json", "ts": "2026-10-12T09:05:00Z"}]
reader_cache = {}
with patch.object(panel, "results_snapshot", return_value=(production_report, ([], indexed_events))) as collected, \
        patch.object(panel.panel_reader, "event_tail", return_value=([{"attempt_id": "call-attempt",
               "event_type": "attempt.created", "ts": "2026-10-12T09:00:00Z"}], True)) as event_read, \
        patch.object(panel_results, "_records", return_value=([], indexed_events)), \
        patch.object(panel_results, "_indexed", return_value=b'{"receipt":{"status":"sent"}}'), \
        patch.object(panel, "verified_patch", return_value=patch_text):
    result_view = panel.render_call_view(project, "worker-1", "Summary", 100, cache=reader_cache)
    timeline_view = panel.render_call_view(project, "worker-1", "Events", 100, cache=reader_cache)
    production_diff = panel.render_call_view(project, "worker-1", "Diff", 100, cache=reader_cache)
assert collected.call_count == 1 and event_read.call_count == 1, (collected.call_count, event_read.call_count)
assert "Summary" in result_view and "Events" in timeline_view and "Diff" in production_diff
assert "timeline truncated" in timeline_view and "review · Reviewed the change" in timeline_view and "[GATE not recorded]" in timeline_view
assert "patch-land · Landed" in timeline_view and "panel-delivery" in timeline_view and "[sent]" in timeline_view, timeline_view
diff_cache = {}
with patch.object(panel, "results_snapshot", return_value=(production_report, ([], []))) as collected, \
        patch.object(panel.panel_reader, "event_tail") as event_read, \
        patch.object(panel, "verified_patch", side_effect=ValueError("digest mismatch")):
    unreadable_diff = panel.render_call_view(project, "worker-1", "Diff", 100, cache=diff_cache)
assert collected.call_count == 1 and not event_read.called, (collected.call_count, event_read.call_count)
assert "patch unreadable (digest mismatch / read error)" in unreadable_diff, unreadable_diff
words = shlex.split(panel.reader_shell(Path("/repo x"), ("result", "worker-1"), "%1"))
assert words[:2] == ["exec", "env"] and "OMS_PANEL_READER_BOARD=%1" in words, words
assert words[-5:] == ["--repo", "/repo x", "--results", "--participant", "worker-1"], words
mail_words = shlex.split(panel.reader_shell(Path("/repo x"), ("mail", "main-1"), "%1", "room-1"))
assert mail_words[-6:] == ["--repo", "/repo x", "--mail", "main-1", "--room", "room-1"], mail_words
assert panel.read_bottom(panel.bottom_text({"tab": "detail", "target": ("mail", "main-1"),
                                          "offset": 0, "dismissed": False}))["target"] == ("mail", "main-1")
def reader_calls(listing, native="%2", fail=()):
    def answer(command, **kwargs):
        out = {"list-panes": listing, "show-option": native + "\n", "split-window": "%9\n"}.get(command[1], "")
        return subprocess.CompletedProcess(command, 1 if command[1] in fail else 0, out, "")
    with patch.object(panel_reader.subprocess, "run", side_effect=answer) as tmux_run:
        opened = panel_reader.open_reader("%1", "CMD", Path("/repo"))
    return opened, [call.args[0][1:] for call in tmux_run.call_args_list]
opened, calls = reader_calls("%1\t40\t\n%2\t40\t\n")
assert opened == "%9" and calls[-2:] == [
    ["split-window", "-d", "-v", "-b", "-l", "24", "-t", "%2", "-c", "/repo", "-P", "-F", "#{pane_id}", "CMD"],
    ["set-option", "-p", "-t", "%9", "@oms_panel_reader", "%1"]], calls
opened, calls = reader_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n")
assert opened == "%9" and calls[-1] == ["respawn-pane", "-k", "-t", "%9", "-c", "/repo", "CMD"], calls
assert not any(c[0] in ("split-window", "kill-pane") or "%2" in c for c in calls), calls
opened, calls = reader_calls("%1\t40\t\n%8\t24\t%7\n%2\t15\t\n")
assert opened == "%9" and calls[-2][:7] == ["split-window", "-d", "-v", "-b", "-l", "9", "-t"], "another board's reader was reused"
for listing, native in (("%1\t20\t\n%2\t12\t\n", "%2"), ("%1\t40\t\n%2\t40\t\n", ""), ("%1\t40\t\n%2\t40\t\n", "%1"),
                        ("%1\t40\t\n%2\t40\t\n", "%5"), ("%1\t40\t\n%2\t40\t\n", "%2;kill")):
    opened, calls = reader_calls(listing, native)
    assert opened is None and not any(c[0] == "split-window" for c in calls), (listing, native, calls)
opened, calls = reader_calls("%1\t40\t\n%2\t40\t\n", fail=("set-option",))
assert opened is None and calls[-1] == ["kill-pane", "-t", "%9"], "an unmarked reader was left behind"
for failure in (subprocess.TimeoutExpired("tmux", 3), OSError("tmux unavailable")):
    for cleanup_failure in (None, subprocess.TimeoutExpired("tmux", 3), OSError("tmux gone")):
        def interrupted_reader(command, **kwargs):
            assert kwargs["timeout"] == 3 and kwargs["stdin"] == subprocess.DEVNULL
            if command[1] == "set-option":
                raise failure
            if command[1] == "kill-pane" and cleanup_failure:
                raise cleanup_failure
            out = {"list-panes": "%1\t40\t\n%2\t40\t\n", "show-option": "%2\n", "split-window": "%9\n"}.get(command[1], "")
            return subprocess.CompletedProcess(command, 0, out, "")
        with patch.object(panel_reader.subprocess, "run", side_effect=interrupted_reader) as tmux_run:
            try:
                panel_reader.open_reader("%1", "CMD", Path("/repo"))
            except (OSError, subprocess.TimeoutExpired) as error:
                assert error is failure, "cleanup replaced the original failure"
            else:
                raise AssertionError("reader marking failure was swallowed")
        assert tmux_run.call_args.args[0] == ["tmux", "kill-pane", "-t", "%9"], "an unmarked reader was left behind"
assert reader_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n", fail=("respawn-pane",))[0] is None
assert panel_reader.split_rows(13) is None and panel_reader.split_rows(14) == 8 and panel_reader.split_rows(40) == 24
def close_calls(listing, board="%1"):
    with patch.object(panel_reader.subprocess, "run", side_effect=lambda command, **kwargs: subprocess.CompletedProcess(
            command, 0, listing if command[1] == "list-panes" else "", "")) as tmux_run:
        closed = panel_reader.close_reader(board)
    return closed, [call.args[0][1:] for call in tmux_run.call_args_list]
assert close_calls("%1\t40\t\n%9\t24\t%1\n%2\t15\t\n") == (True, [
    ["list-panes", "-t", "%1", "-F", "#{pane_id}\t#{pane_height}\t#{@oms_panel_reader}"], ["kill-pane", "-t", "%9"]])
assert close_calls("%1\t40\t\n%2\t40\t\n")[0] is False and close_calls("x", board="1; kill")[1] == []
# Completion at the same terminal size and line count is repainted without losing the person's scroll.
def read_page(render, batches, on_refresh=None, size=(80, 4)):
    clock, waits = [0.0], []
    class PageInput:
        def __enter__(self):
            return self
        def __exit__(self, *args):
            pass
        def wait(self, seconds):
            assert 0 < seconds <= 1
            waits.append(seconds)
            clock[0] += seconds
            return next(batches)
    with patch.object(panel_reader, "TerminalInput", return_value=PageInput()), \
            patch.object(panel_reader.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel_reader.shutil, "get_terminal_size", return_value=os.terminal_size(size)), \
            patch.object(panel_reader, "tmux", return_value=subprocess.CompletedProcess([], 0, "%1\n", "")) as tmux_run, \
            patch.object(panel_reader.sys, "stdout", io.StringIO()) as output:
        assert panel_reader.page(render, "%1", on_refresh=on_refresh) == 0
    assert tmux_run.call_args.args == ("select-pane", "-t", "%1")
    return output.getvalue(), waits
page_cache = {}
with patch.object(panel, "results_snapshot", side_effect=[(running_report, ([], [])),
                                                            (finished_report, ([], []))]) as collected:
    output, waits = read_page(
        lambda width, view: panel.render_call_view(project, "worker-1", view, width, cache=page_cache),
        iter([[], [], [("q",)]]), on_refresh=page_cache.clear)
assert collected.call_count == 2 and "finished" in output.lower() and waits == [1, 1, 1], (
    collected.call_count, waits, output)
with patch.object(panel, "render_results", side_effect=["\n".join("running %d" % i for i in range(8)),
                                                       "\n".join("done %d" % i for i in range(8))]) as rendered:
    output, waits = read_page(rendered, iter([[("pagedown",)], [], [("q",)]]))
assert rendered.call_count == 2 and waits == [1, 1, 1], "reader evidence polling was unbounded or stale"
assert "done 3\ndone 4\ndone 5" in output and output.count("4-6/8") == 2, output
with patch.object(panel, "render_results", side_effect=["\n".join(str(i) for i in range(8)), "short"]) as rendered:
    output, unused = read_page(rendered, iter([[("end",)], [], [("escape",)]]))
assert "short" in output and "rows 1-1/1" in output, "shrinking evidence left the reader out of range"
failure = "bad\033]52;c;payload\a\x9b31m\u200b\r\n" + "x" * 250
with patch.object(panel, "render_results", side_effect=ValueError(failure)) as rendered:
    output, unused = read_page(rendered, iter([[("q",)]]))
assert "[✗ unavailable] bad?]52;c;payload" in output, output
assert "\033]52" not in output and not any(c in output for c in ("\a", "\x9b", "\u200b", "\r")), output
output, unused = read_page(lambda width: "\033[32mspoof\033[0m", iter([[("q",)]]))
assert "\033[32mspoof" not in output and "?[32mspoof" in output, output
with patch.object(panel, "render_results", side_effect=["first\nsecond", ValueError("refresh lost")]) as rendered:
    output, unused = read_page(rendered, iter([[], [], [("q",)]]), size=(80, 10))
assert "showing earlier content" in output and "first" in output and "second" in output, output
# A slow result query runs off the input thread: q closes the reader at once, showing the pending notice.
slow_started = time.time()
output, unused = read_page(lambda width: (time.sleep(3), "late")[1], iter([[("q",)]]))
assert time.time() - slow_started < 2 and "Reading recorded result…" in output, output
selected_views = []
def switching_reader(width, view):
    selected_views.append(view)
    return panel_reader.render_view(view, {"rows": []}, read_call)
output, unused = read_page(switching_reader, iter([[('tab',)], [('r',)], [('e',)], [('d',)], [('q',)]]))
assert selected_views[0] == "Summary" and "Events" in selected_views and "Diff" in selected_views, selected_views
assert "No result recorded yet" in output and "No retained events" in output and "No patch recorded" in output, output
output, unused = read_page(lambda width, view: "[Summary]  Events  Diff\nA dispatched call",
                           iter([[('q',)]]), size=(80, 10))
assert "[⟳ starting]" in output and "status unknown" not in output, output
long_report = {"rows": [{"role": "worker", "title": "Pinned reader", "process_state": "working",
                         "task_id": "reader-task"}]}
long_call = {"answer": "\n".join("line %d" % i for i in range(20))}
output, unused = read_page(lambda width, view: panel_reader.render_view(view, long_report, long_call),
                           iter([[("pagedown",)], [("q",)]]), size=(80, 10))
assert output.count("WORKER model unknown · Pinned reader") >= 2 and output.count("[Summary]") >= 2, output
assert "rows 1-" in output and "updated unknown" in output, output
output, unused = read_page(lambda width, view: panel_reader.render_view(view, long_report, markdown_call),
                           iter([[("tab",)], [("q",)]]), size=(80, 10))
raw_frame = output.split("\033[H\033[2J")[-1]
assert raw_frame.count("[Raw]") == 1 and "# Heading" in raw_frame, raw_frame
# A room transition closes the reader through the watcher's close path; refreshes of the same room do not.
class RoomFrameReceived(Exception):
    pass
for current_room in ("old-room", "new-room"):
    def room_frame(*args, **kwargs):
        assert closed.call_count == int(current_room != "old-room"), "room refresh left the reader open or closed it unnecessarily"
        if current_room != "old-room":
            closed.assert_called_once_with("%1")
            assert "preview" not in kwargs["navigation"]
        raise RoomFrameReceived()
    with patch.dict(os.environ, {"TMUX": "", "TMUX_PANE": "%1", "OMS_PANEL_RESUME": json.dumps(
            {"room_id": "old-room", "preview": ["result", "w1"]})}), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "session_option", return_value=""), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel, "read_panel", return_value=({"room": {"id": current_room}}, 0, "codex", None,
                                                           "graph", False, current_room)), \
            patch.object(panel, "frame_view", side_effect=room_frame), \
            patch.object(panel_reader, "close_reader", return_value=True) as closed:
        try:
            panel.watch(Path("."), "codex", count=1)
        except RoomFrameReceived:
            pass
# Tree calls can open the reader; graph clicks and Enter stay in the board's detail area.
chosen_nav = {"tab": "detail", "preview": {"target": ("result", "w1")}, "selected": ("result", "w1"),
              "hits": [{"y": 3, "x1": 1, "x2": 9, "action": ("result", "w1")}, {"y": 4, "x1": 1, "x2": 9, "action": ("group", "g")}]}
assert panel_reader.picked(("enter",), chosen_nav) == ("result", "w1") == panel_reader.picked(("click", 2, 3), chosen_nav)
assert panel_reader.picked(("click", 2, 4), chosen_nav) is None and panel_reader.picked(("down",), chosen_nav) is None
assert panel_reader.picked(("enter",), dict(chosen_nav, preview={"target": ("chat", "m1")}, selected=("chat", "m1"))) == ("chat", "m1")
for target in (("result", "w1"), ("debate", ("task", "seat")), ("chat", "m1")):
    for event in (("click", 2, 3), ("enter",)):
        graph_picked = dict(chosen_nav, surface="graph", selected=target, preview={"target": target},
                            hits=[{"y": 3, "x1": 1, "x2": 9, "action": target}])
        assert panel_reader.picked(event, graph_picked) is None, (event, target)
        assert panel_reader.picked(event, dict(graph_picked, surface="tree")) == target, (event, target)
# A reader pane is never discovered as a board or an input pane, so --reopen ignores it.
reader_row = "oms-fixture\t@1\t%%3\t0\t%%1\texec bash %s panel --repo /repo --results --participant w1" % panel.ENTRY
board_row = "oms-fixture\t@1\t%%1\t0\t\texec bash %s panel --repo /repo --watch" % panel.ENTRY
with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, reader_row + "\n" + board_row + "\n", "")):
    assert [row["pane"] for row in panel.panel_panes(Path("/repo"))] == ["%1"]
# A watcher restart keeps the person's selection: watch() restores what the reload saved.
resumed_nav = {}
class StopWatch(Exception):
    pass
def capture_frame(*args, **kwargs):
    resumed_nav.update(kwargs["navigation"])
    raise StopWatch()
with patch.dict(os.environ, {"OMS_PANEL_RESUME": json.dumps({"room_id": "r1", "selected": ["chat", "m2"], "dismissed": True,
                                                            "preview": ["debate", ["t1", "m2"]]})}), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel, "read_panel", return_value=({"room": {"id": "r1"}}, 0, "codex", None, "graph", False, "r1")), \
        patch.object(panel, "frame_view", side_effect=capture_frame):
    try:
        panel.watch(Path("."), "codex", count=1)
    except StopWatch:
        pass
assert resumed_nav["selected"] == ("chat", "m2") and resumed_nav["dismissed"] is True, resumed_nav
assert resumed_nav["preview"]["target"] == ("debate", ("t1", "m2")), resumed_nav
# A watcher restarts only for changed sources that hold still, compile and import in a fresh interpreter.
stamps_now = panel.source_stamps()
assert panel.reload_ready(stamps_now) is False
clock = [100.0]
panel.REJECTED_RELOAD.clear()
with patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]):
    assert panel.reload_ready({}) is False      # first sighting only starts the clock
    clock[0] += .5
    assert panel.reload_ready({}) is False      # not yet one second apart
    clock[0] += .6
    assert panel.reload_ready({}) is True
    with patch.object(panel, "compile", side_effect=SyntaxError("broken"), create=True):
        assert panel.reload_ready({}) is False
    # A tree that compiles but cannot be imported (a half-copied install) keeps the old watcher running.
    broken_tree = temporary / "broken-reload"
    shutil.copytree(root / "scripts" / "lib", broken_tree / "scripts" / "lib", ignore=shutil.ignore_patterns("__pycache__"))
    with patch.object(panel, "ROOT", broken_tree):
        (broken_tree / "scripts" / "lib" / "room_view.py").write_text("from panel_view import no_such_name\n", encoding="utf-8")
        panel.REJECTED_RELOAD.clear()
        assert panel.reload_ready({}) is False
        clock[0] += 1.1
        assert panel.reload_ready({}) is False and panel.REJECTED_RELOAD.pop("notice") is True
        clock[0] += 1.1
        assert panel.reload_ready({}) is False and "notice" not in panel.REJECTED_RELOAD
    panel.REJECTED_RELOAD.clear()
    # Sources still changing between two reads are not reloaded yet.
    unstable = root / "scripts" / "lib" / "panel_view.py"
    for pair in ([{unstable: 1}, {unstable: 2}, {unstable: 2}, {unstable: 2}],):
        with patch.object(panel, "source_stamps", side_effect=pair), patch.object(panel, "compile", create=True):
            assert panel.reload_ready({}) is False
            clock[0] += 1.1
            assert panel.reload_ready({}) is False   # changed again since the first sighting
panel.REJECTED_RELOAD.clear()
# A finished worker's kept worktree opens only when its label names exactly one git worktree.
kept_parent = temporary / "oh-my-setting-delegate.fixture"
kept_parent.mkdir()
subprocess.run(["git", "-C", str(project), "worktree", "add", "--detach", str(kept_parent / "wt")],
               check=True, capture_output=True)
kept = {"participant": "call-kept", "role": "worker", "location": "worktree:oh-my-setting-delegate.fixture/wt"}
assert panel.worker_worktree(project, dict(kept, state="done")) == (kept_parent / "wt").resolve()
for member, reason in ((dict(kept, state="working"), "still running"),
                       (dict(kept, state="done", location="worktree:gone.parent/wt"), "cleaned up"),
                       (dict(kept, state="done", location="repository"), "recorded no worktree")):
    try:
        panel.worker_worktree(project, member)
    except ValueError as error:
        assert reason in str(error), error
    else:
        raise AssertionError("worktree shell opened for " + repr(member))
assert TerminalInput().decode(b"w") == [("w",)]
with patch("room_view.nodes", return_value=[dict(kept, state="done")]), \
        patch.dict(os.environ, {"TMUX": ""}):
    assert panel.open_worktree(project, {}, {"selected": ("result", "call-kept")}) == "Worktree: " + str((kept_parent / "wt").resolve())
subprocess.run(["git", "-C", str(project), "worktree", "remove", "--force", str(kept_parent / "wt")], check=True)
# The board's advisor action opens a question prompt on the control path; it never calls a model itself.
assert TerminalInput().decode(b"a") == [("a",)]
with patch.dict(os.environ, {"TMUX": ""}):
    hint = panel.ask_advisor(Path("/tmp/project-a"), mailed, detail_nav)
assert "--main flow-main" in hint and "--dispatch advisor" in hint, hint
# Inside a user's own tmux session (no OMS panel session) the board opens no popup or window.
with patch.dict(os.environ, {"TMUX": "fixture", "TMUX_PANE": "%9", "OMS_PANEL_SESSION": ""}), \
        patch.object(panel.subprocess, "Popen") as foreign_popup, patch.object(panel.subprocess, "run") as foreign_run:
    assert panel.ask_advisor(Path("/tmp/project-a"), mailed, detail_nav).startswith("Ask from a terminal: ")
    foreign_popup.assert_not_called()
with patch.dict(os.environ, {"TMUX": "fixture", "TMUX_PANE": "%9", "OMS_PANEL_SESSION": "oms-panel-abcdef012345"}), \
        patch.object(panel.subprocess, "Popen") as popup:
    # Without a selected main, the shown main is the target.
    assert "flow-main" in panel.ask_advisor(Path("/tmp/project-a"), mailed, dict(detail_nav, selected=None))
    argv = popup.call_args.args[0]
    assert argv[:2] == ["tmux", "display-popup"] and argv[argv.index("-t") + 1] == "%9", argv
    assert "-u OMS_ROOM_PARTICIPANT" in argv[-1] and "--main flow-main" in argv[-1], argv
    assert "Advisor call failed" in argv[-1] and "read -r _" in argv[-1], "a failed call closed its popup"
    assert argv[-1].startswith("sh -c "), "popup script depends on the user's default shell"
try:
    panel.ask_advisor(Path("/tmp/project-a"), mailed, {"room_id": "flow-room"})
except ValueError as error:
    assert "select a joined main" in str(error)
else:
    raise AssertionError("advisor action ran without a selected main")
with patch.object(panel.sys.stdin, "isatty", return_value=False), patch.dict(os.environ, {}, clear=False):
    from contextlib import redirect_stderr
    error_text = io.StringIO()
    with redirect_stderr(error_text):
        assert panel.main(["--repo", str(root), "--dispatch", "advisor", "--owner", "claude", "--main", "flow-main"]) == 1
    assert "advisor dispatch needs --prompt or an interactive terminal" in error_text.getvalue(), error_text.getvalue()
with patch.object(panel, "session_owner", return_value="/elsewhere"):
    try:
        panel.split_panel(Path("/tmp/project-a"))
    except ValueError as error:
        assert "not this checkout" in str(error)
    else:
        raise AssertionError("a foreign panel session was reused")
assert panel.resolve_session(Path("/tmp/project-a"), wait=False)[0] == panel.resolve_session(Path("/tmp/project-a/."), wait=False)[0]
# A launch that loses the creation race joins the winner without treating its own new room as a choice.
race_repo = project
original_split = panel.split_panel
seen = {}
with patch.dict(os.environ, {"OMS_ROOM_ID": ""}), patch.object(panel, "session_owner", side_effect=[None, None, str(race_repo), str(race_repo)]), \
        patch.object(panel, "legacy_session", return_value=None), patch.object(panel, "check_legacy_main"), \
        patch.object(panel, "ensure_room", return_value="room-lost"), \
        patch.object(panel.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, ["tmux"])), \
        patch.object(panel, "split_panel", side_effect=lambda *a, **k: seen.update(room=os.environ.get("OMS_ROOM_ID"), retry=k.get("retry")) or 0):
    os.environ.pop("OMS_ROOM_ID")
    assert original_split(race_repo, launch="codex") == 0
assert seen == {"room": None, "retry": False}, seen
# A new panel opens only Claude, sharing the room recorded before the owner marker.
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-both"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.shutil, "which", return_value="/fixture/bin"), \
        patch.object(panel.subprocess, "check_output", side_effect=["%1\n", "%2\n", "%3\n"]) as created_panes, \
        patch.object(panel, "check_legacy_main"), \
        patch.object(panel.subprocess, "run") as tmux_calls, patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
    os.environ.pop("OMS_ROOM_ID")
    os.environ.pop("OMS_PANEL_MAIN", None)
    assert panel.split_panel(race_repo) == 0
    windows = [c.args[0] for c in created_panes.call_args_list]
    assert windows[0][1] == "new-session" and [w[w.index("-n") + 1] for w in windows[1:]] == ["claude"], windows
    order = [c.args[0][1:5] for c in tmux_calls.call_args_list]
    assert ["set-environment", "-t", "=" + panel.resolve_session(race_repo, wait=False)[0], "OMS_ROOM_ID"] in order
    assert any(c.args[0][1:3] == ["select-window", "-t"] and c.args[0][3] == "%2" for c in tmux_calls.call_args_list)
with patch.dict(os.environ, {"OMS_PANEL_MAIN": ""}), patch.object(panel.sys, "stderr", new_callable=io.StringIO) as warnings:
    assert panel.panel_mains() == []
    assert warnings.getvalue() == ""
for invalid in ("claude,pi", "unknown"):
    with patch.dict(os.environ, {"OMS_PANEL_MAIN": invalid}), patch.object(panel.shutil, "which", return_value="/x"), \
            patch.object(panel.sys, "stderr", new_callable=io.StringIO) as warnings:
        assert panel.panel_mains() == []
        assert warnings.getvalue().splitlines() == [
            "Warning: OMS_PANEL_MAIN accepts one of claude, pi, codex (or empty to open no main); no main opened."]
for configured, available, expected in ((None, {"claude", "pi", "codex"}, ["claude"]),
                                      (None, {"pi", "codex"}, ["pi"]),
                                      (None, {"pi"}, []),
                                      (None, {"codex"}, ["codex"]),
                                      ("pi", {"claude", "pi", "codex"}, ["pi"]),
                                      ("claude", {"pi", "codex"}, [])):
    with patch.dict(os.environ, {}, clear=False), \
            patch.object(panel.shutil, "which", side_effect=lambda name: "/x/" + name if name in available else None):
        if configured is None:
            os.environ.pop("OMS_PANEL_MAIN", None)
        else:
            os.environ["OMS_PANEL_MAIN"] = configured
        assert panel.panel_mains() == expected, (configured, available, panel.panel_mains())
# A session whose owner marker is not written yet is waited for, not refused as foreign.
with patch.object(panel, "session_owner", side_effect=["", str(race_repo), str(race_repo)]), patch.object(panel.time, "sleep"), \
        patch.object(panel.subprocess, "call", return_value=0):
    assert panel.split_panel(race_repo) == 0
with patch.object(panel, "session_owner", return_value=None), patch.object(panel, "legacy_session", return_value=None), \
        patch.object(panel, "ensure_room", return_value="room-first"), patch.object(panel, "board_split"), \
        patch.object(panel, "bind_window"), patch.object(panel.subprocess, "call", return_value=0), \
        patch.object(panel.subprocess, "check_output", return_value="%1\n"), \
        patch.object(panel, "check_legacy_main"), \
        patch.object(panel.subprocess, "run") as tmux_calls, patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
    os.environ.pop("OMS_ROOM_ID")
    assert panel.split_panel(race_repo, launch="codex") == 0
    order = [c.args[0][1:4] for c in tmux_calls.call_args_list]
    assert order.index(["set-environment", "-t", "=" + panel.resolve_session(race_repo, wait=False)[0]]) < \
        next(i for i, a in enumerate(order) if a[0] == "set-option"), order
# One proven legacy random-named panel is adopted; several are never merged.
legacy_rows = [{"session": "oms-panel-aaaaaaaaaaaa"}, {"session": "oms-panel-aaaaaaaaaaaa"}]
with patch.object(panel, "panel_panes", return_value=legacy_rows):
    assert panel.legacy_session(Path("/tmp/project-a")) == "oms-panel-aaaaaaaaaaaa"
with patch.object(panel, "panel_panes", return_value=legacy_rows + [{"session": "oms-panel-bbbbbbbbbbbb"}]):
    assert panel.legacy_session(Path("/tmp/project-a")) is None
assert panel.resolve_session(Path("/tmp/project-a"), wait=False)[0] == "oms-project-a"
# The slug is lowercase [a-z0-9-]; a taken name falls to the hashed one, never over another checkout or a non-OMS session.
import tempfile
with tempfile.TemporaryDirectory() as slug_root:
    odd = Path(slug_root) / "  My.Project_X v2  "
    odd.mkdir()
    plain, hashed = panel.session_candidates(odd)
    assert plain == "oms-my-project-x-v2" and re.fullmatch(r"oms-my-project-x-v2-[a-f0-9]{6}", hashed), (plain, hashed)
    long_name = Path(slug_root) / ("A" * 40)
    long_name.mkdir()
    assert panel.session_candidates(long_name)[0] == "oms-" + "a" * 32
    bare = Path(slug_root) / "..."
    bare.mkdir()
    assert panel.session_candidates(bare)[0] == "oms-project"
    assert re.fullmatch(panel.PANEL_SESSION, hashed) and re.fullmatch(panel.PANEL_SESSION, "oms-panel-abcdef012345")
    for held, expected in ((None, plain), (str(odd.resolve()), plain), ("/elsewhere", hashed), ("", hashed)):
        with patch.object(panel, "session_owner", side_effect=lambda n, h=held: h if n == plain else None):
            assert panel.resolve_session(odd, wait=False) == (expected, held if expected == plain else None), held
    with patch.object(panel, "session_owner", return_value="/elsewhere"):
        assert panel.resolve_session(odd, wait=False) == (hashed, "/elsewhere")
    # A live legacy panel proven for this checkout is adopted while the new name is free.
    with patch.object(panel, "session_owner", side_effect=lambda n: None if n == plain else str(odd)), \
            patch.object(panel, "legacy_session", return_value="oms-panel-aaaaaaaaaaaa"), \
            patch.object(panel, "bind_main_keys"), patch.object(panel.subprocess, "run") as adopted, \
            patch.object(panel.subprocess, "call", return_value=0):
        assert panel.split_panel(odd) == 0
        assert ["tmux", "set-option", "-t", "=oms-panel-aaaaaaaaaaaa:", "@oms_panel_repo", str(odd)] in \
            [c.args[0] for c in adopted.call_args_list]
# Context used rides on the shown main's card in three bands; only the proven main carries it.
claude_main = next(m for m in flow["room"]["participants"] if m.get("role") == "main" and m.get("provider") == "claude")["participant"]
for left, words in ((42, "ctx 58% used"), (22, "ctx 78% used"), (12, "ctx 88% used")):
    metered = dict(flow, provider_status={"claude": {"participant": claude_main, "context_left": left}})
    picture = render(metered, "codex", 110, 30, view="graph", navigation={"selected": ("chat", claude_main)})
    # The usage row already says "ctx N% used"; the card is the second place.
    assert picture.count(words) == 1, (left, picture)
    plain = render(metered, "codex", 110, 30, view="graph", unicode=False, navigation={"selected": ("chat", claude_main)})
    assert plain.count(words.replace("·", "/")) == 1 and not any(g in plain for g in "─│·"), (left, plain)
    note = sys.modules["room_view"].context_note(metered, next(m for m in flow["room"]["participants"] if m["participant"] == claude_main))
    assert note == (100 - left, "alert" if left <= 15 else "dim" if left > 30 else None), note
picture_80x40 = render(dict(flow, provider_status={"claude": {"participant": claude_main, "context_left": 10}}),
                       "codex", 80, 40, view="graph", navigation={"selected": ("chat", claude_main)})
assert "ctx 90% used" in picture_80x40, picture_80x40
assert render(dict(flow, provider_status={"claude": {"participant": "someone-else", "context_left": 42}}), "codex", 110, 30,
              view="graph", navigation={"selected": ("chat", claude_main)}).count("ctx 58% used") == 0
# The board shows only its own main's calls, expanded or not.
for board_nav in ({}, {"expanded": True}):
    assert "ONLY OTHER OWNER" not in render(flow, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=board_nav)
# Page keys move graph bands (or the shown detail) rather than an offset the bands ignore.
page_nav = {}
render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
choose(("pagedown",), page_nav)
assert page_nav["band_offsets"]["detail"] == 6, "a shown detail takes the page keys"
page_nav = {"dismissed": True}
render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
choose(("pagedown",), page_nav)
assert page_nav["band_offsets"]["calls"] == 3, page_nav["band_offsets"]
# A results screen opened from the graph scrolls itself, not the graph's stale bands.
from panel_tree import render_detail
from panel_view import message_rows
wrapped_message = message_rows("From S: " + "x" * 30, 18)
assert len(wrapped_message) > 1 and not wrapped_message[1].startswith(" "), wrapped_message
indented_message = message_rows("From S: " + "x" * 30, 18, indent=len("From S: "))
assert len(indented_message) > 1 and indented_message[1].startswith(" " * len("From S: ")), indented_message
assert all(display_width(line) <= 18 for line in indented_message), indented_message
cjk_message = message_rows("漢字かな交じり文" * 4, 9)
assert len(cjk_message) > 1 and all(display_width(line) <= 9 for line in cjk_message), cjk_message
assert "".join(cjk_message) == "漢字かな交じり文" * 4, cjk_message
late_colon = message_rows("first line\nthe panel already has a live main or unverified owner; if crashed: oms panel --release-main att_x", 50)
assert len(late_colon) <= 4 and all(display_width(line) <= 50 for line in late_colon), late_colon
assert "".join(late_colon) == "first linethe panel already has a live main or unverified owner; if crashed: oms panel --release-main att_x", late_colon
long_prefix = message_rows("Last: " + "漢" * 20, 7, indent=6)
assert all(display_width(line) <= 7 for line in long_prefix), long_prefix
assert "".join(long_prefix) == "Last: " + "漢" * 20, long_prefix
detail_fixture = {"title": "Failure", "text": "## Result\nNarrative mentions failed and ok.\nOutcome: failed\nStage: running"}
colored_detail = render_detail({}, detail_fixture, 80, 20, {}, color=True)
assert "\033[" in colored_detail and "\033[" in colored_detail.splitlines()[0], colored_detail
assert re.search(r"\033\[[0-9;]*mOutcome:\033\[0m", colored_detail), colored_detail
assert re.search(r"\033\[[0-9;]*mfailed\033\[0m", colored_detail), colored_detail
assert "\033[" not in next(line for line in colored_detail.splitlines() if line.startswith("Narrative")), colored_detail
assert "\033[" not in "\n".join(colored_detail.splitlines()[-2:]), colored_detail
for tiny_height in (1, 2):
    tiny_detail = render_detail({}, detail_fixture, 80, tiny_height, {}, color=True)
    assert "\033[" not in tiny_detail, tiny_detail
with patch.dict(os.environ, {"NO_COLOR": ""}):
    assert "\033[" not in render_detail({}, detail_fixture, 80, 20, {}, color=True)
page_nav["surface"] = "inbox"  # A reader opened from the inbox must still receive page and wheel events.
page_nav["detail"] = {"title": "RECORDED RESULTS", "text": "\n".join("line %s" % n for n in range(300))}
render_detail(flow, page_nav["detail"], 110, 30, page_nav)
bands_before = dict(page_nav["band_offsets"])
choose(("down",), page_nav)
assert page_nav["offset"] == 1 and page_nav["band_offsets"] == bands_before
choose(("end",), page_nav)
assert "line 299" in render_detail(flow, page_nav["detail"], 110, 30, page_nav)
choose(("down",), page_nav)
assert "line 299" in render_detail(flow, page_nav["detail"], 110, 30, page_nav)
choose(("home",), page_nav)
choose(("up",), page_nav)
assert page_nav["offset"] == 0
page_nav["detail"]["text"] = "\n".join("long result line %s with retained information" % n for n in range(300))
render_detail(flow, page_nav["detail"], 110, 30, page_nav)
wide_rows = page_nav["body_rows"]
render_detail(flow, page_nav["detail"], 24, 20, page_nav)
assert page_nav["body_rows"] > wide_rows
choose(("end",), page_nav)
assert "information" in render_detail(flow, page_nav["detail"], 24, 20, page_nav)
choose(("home",), page_nav)
choose(("pagedown",), page_nav)
choose(("scroll", 3, 5, 10), page_nav)
assert page_nav["offset"] > 0 and page_nav["bands"] == [], page_nav["offset"]
assert "EXTRA WORKER 2" in render(crowded, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation=page_nav)
try:
    panel.main(["--repo", str(root), "--dispatch", "advisor", "--owner", "claude", "--main", "a b", "--dry-run", "--prompt", "x"])
except SystemExit as error:
    assert error.code == 2
else:
    raise AssertionError("an invalid --main was accepted")
# Each snapshot refreshes an open preview; another room resets the view.
kept_nav = {"room_id": "flow-room", "tab": "detail", "preview": {"target": ("result", "builder"), "report": {}}}
assert panel.refreshed_navigation(kept_nav, flow)["preview"]["refresh"] is True
assert "preview" not in panel.refreshed_navigation(dict(kept_nav, room_id="other"), flow)
# The bottom box is one panel-session state: a tab, target or scroll changed on one board reaches the others.
session_bottom = {}
def fake_bottom_tmux(command, **kwargs):
    if "@oms_panel_bottom" in command:
        session_bottom["raw"] = command[command.index("@oms_panel_bottom") + 1]
    return subprocess.CompletedProcess(command, 0, "", "")
fresh_bottom = {"tab": "detail", "target": None, "offset": 0, "dismissed": False}
board_a = {"room_id": "flow-room", "tab": "plan", "preview": {"target": ("debate", ("t1", "flow-other"))},
           "band_offsets": {"detail": 3}}
with patch.object(panel, "managed_session", return_value=True), patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-panel-x"}), \
        patch.object(panel.subprocess, "run", side_effect=fake_bottom_tmux) as bottom_writes:
    panel.publish_bottom(board_a, panel.bottom_state(board_a))
    assert bottom_writes.call_count == 0, "an unchanged bottom box was written"
    panel.publish_bottom(board_a, fresh_bottom)
    assert bottom_writes.call_count == 1 and len(session_bottom["raw"]) <= 512, session_bottom
    assert panel.read_bottom(session_bottom["raw"]) == panel.bottom_state(board_a) == {
        "tab": "plan", "target": ("debate", ("t1", "flow-other")), "offset": 3, "dismissed": False}, session_bottom
board_b = {"room_id": "flow-room", "tab": "messages", "preview": {"target": ("chat", "flow-main")}}
panel.refreshed_navigation(board_b, flow, session_bottom["raw"])
assert board_b["tab"] == "plan" and board_b["preview"]["target"] == ("debate", ("t1", "flow-other")), board_b
assert board_b["band_offsets"]["detail"] == 3 and "selected" not in board_b, board_b
board_b["tab"] = "debate"  # B's own later change is not undone while the shared value is unchanged
assert panel.refreshed_navigation(board_b, flow, session_bottom["raw"])["tab"] == "debate"
shared_tab = lambda target: json.dumps({"tab": "messages", "target": target, "offset": 0})
assert "preview" not in panel.refreshed_navigation(board_b, flow, shared_tab(None))
# Esc in one window empties the box in every window; a later pick clears it again.
emptied = panel.refreshed_navigation({"room_id": "flow-room"}, flow,
                                     json.dumps({"tab": "detail", "target": None, "offset": 0, "dismissed": True}))
assert emptied.get("dismissed") is True and panel.bottom_state(emptied)["dismissed"] is True, emptied
assert "dismissed" not in panel.refreshed_navigation(emptied, flow, shared_tab(["chat", "flow-main"]))
# A target naming nobody in this room is ignored; the tab still follows.
ghost_nav = panel.refreshed_navigation({"room_id": "flow-room", "preview": {"target": ("chat", "flow-main")}},
                                       flow, shared_tab(["chat", "ghost"]))
assert ghost_nav["tab"] == "messages" and ghost_nav["preview"]["target"] == ("chat", "flow-main"), ghost_nav
for invalid in ("", "{", "[]", '{"tab":"nope","target":null}', '{"tab":"plan","target":["x","y"]}',
                '{"tab":"plan","target":["chat","a b"]}', '{"tab":"plan","target":["debate","t1"]}',
                '{"tab":"plan","offset":-1}', '{"tab":"plan","offset":true}', '{"tab":"plan","extra":1}',
                '{"tab":"plan","dismissed":1}', '{"tab":"between","target":null}', shared_tab(["pair", ["flow-main", "flow-other"]]),
                shared_tab(["chat", "x" * 600]), shared_tab(["chat", "x" * 129])):
    assert panel.read_bottom(invalid) is None, invalid
    assert panel.refreshed_navigation({"room_id": "flow-room", "tab": "plan"}, flow, invalid) == {"room_id": "flow-room", "tab": "plan"}
# Arriving at a window never clears the selection.
assert "was_active" not in Path(panel.__file__).read_text(encoding="utf-8")
# A full or closed selected room makes the next main start a new room instead of reusing it.
with patch.dict(os.environ, {"OMS_ROOM_ID": "room-full"}), patch.object(panel, "room_headroom", return_value=False), \
        patch.object(panel.room, "create", return_value="room-fresh") as fresh, \
        patch.object(panel, "managed_session", return_value=False):
    assert panel.panel_room(Path("/tmp/project-a")) == "room-fresh" and fresh.called
# --main without a room cannot fall through to an unowned call.
with patch.dict(os.environ, {"OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": "", "OMS_PANEL_MAIN_ATTEMPT": ""}), \
        patch.object(panel, "child_environment", return_value={"PATH": os.environ.get("PATH", "")}), \
        patch.object(panel.shutil, "which", return_value="/bin/true"), \
        patch.object(panel.subprocess, "call", side_effect=AssertionError("model call without owner")):
    try:
        panel.dispatch(Path("/tmp/project-a"), "claude", "advisor", None, "read", None,
                       prompt="Who owns this?", main="unknown-main")
    except ValueError as error:
        assert "--main needs a room" in str(error)
    else:
        raise AssertionError("--main without a room was ignored")
# UX contract from the critique debate: the quit key survives narrow footers, arrows move the
# detail with the main, hidden tabs keep their attention, and attention rows name their state.
# Footer hints follow the selection (at most five), keep the chat/board toggle and "?" in every width,
# name only keys that work, and "?" swaps in the full key list.
for view_name in ("graph", "tree"):
    for columns in (40, 80, 100, 157):
        for managed_board in (True, False):
            drawn = render(flow, "codex", columns, 24, view=view_name, main_attempt="flow-attempt",
                           navigation={}, managed=managed_board).splitlines()
            # The panel-wide F-keys are fixed at the end of the hint line; board keys fill what is left.
            footer_line = next(line for line in reversed(drawn) if "Keys" in line)
            # The key hints are pinned to the board's last row, however short the content is.
            assert "Keys" in drawn[-1] and len(drawn) == 24, (view_name, columns, drawn[-3:])
            assert display_width(footer_line) <= columns, (view_name, columns, footer_line)
            if columns >= 80:
                assert footer_line.endswith("F12 Keys" if managed_board else "? Keys"), (view_name, columns, footer_line)
            assert ("F9" in footer_line) == managed_board and ("q Quit" in footer_line) == (not managed_board), footer_line
            assert len(re.split(r"  +", footer_line)) <= 7 and "Esc Back" not in footer_line and "q Chat" not in footer_line, footer_line
    # Board footers reserve space for context; F5 stays in full help.
    for control, offered in ((True, True), (False, False)):
        launched = render(flow, "codex", 120, 24, view="graph", main_attempt="flow-attempt",
                          navigation={"control_window": control}, managed=True).splitlines()[-1]
        assert "F5 Control" not in launched and launched.endswith("F12 Keys"), launched
    open_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main")}}
    assert "Esc Close" in render(flow, "codex", 157, 24, view=view_name, main_attempt="flow-attempt",
                                 navigation=open_nav, managed=True).splitlines()[-1]
    for rows_high in (24, 30):
        open_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main")}}
        open_board = render(flow, "codex", 157, rows_high, view=view_name, main_attempt="flow-attempt",
                            navigation=open_nav, managed=True)
        assert "Esc Close" in open_board.splitlines()[-1], open_board
        assert open_nav["tab_body_visible"], open_board
    for expanded_board, label in ((False, "v Expand"), (True, "v Collapse")):
        helped = render(flow, "codex", 100, 24, view=view_name, main_attempt="flow-attempt", managed=True,
                        navigation={"keys_help": True, "expanded": expanded_board}).splitlines()
        assert label in "\n".join(helped) and "? Hide" in "\n".join(helped), helped
        assert all(display_width(line) <= 100 for line in helped), helped
alarmed = deepcopy(flow)
alarmed["room"]["participants"].append({"participant": "third-main", "role": "main", "joined": True,
                                        "provider": "claude", "model": "claude-opus-5-5", "label": "Third"})
alarmed["room"]["participants"].append({"participant": "third-fail", "role": "worker", "joined": True,
                                        "model": "gpt-6-luna", "parent": "third-main", "label": "Broken"})
alarmed["attempts"]["active_recent"] += [
    {"attempt_id": "third-main", "state": "working", "panel": {"room_id": "flow-room", "room_participant": "third-main", "role": "main"}},
    {"attempt_id": "third-fail", "state": "failed", "panel": {"room_id": "flow-room", "room_participant": "third-fail", "role": "worker"}}]
for columns in range(76, 90):
    sweep_nav = {}
    swept = render(alarmed, "codex", columns, 24, view="graph", main_attempt="flow-attempt", navigation=sweep_nav)
    assert all(display_width(line) <= columns for line in swept.splitlines()), (columns, swept)
    assert all(hit["x2"] <= columns for hit in sweep_nav["hits"]), columns
# Finished calls leave the live drawing for the Past tab; its count and inspection remain.
finished_flow = deepcopy(flow)
for attempt in finished_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
finished_picture = render(finished_flow, "codex", 120, 34, view="graph", navigation={"dismissed": True})
assert "WORKER Sonnet" not in finished_picture and "Past work" not in finished_picture \
    and "Past 1" in finished_picture, finished_picture
lines_of = finished_picture.splitlines()
assert next(i for i, l in enumerate(lines_of) if "ADVISOR Astra" in l) < next(i for i, l in enumerate(lines_of) if "MAIN Sol 6" in l) \
    < next(i for i, l in enumerate(lines_of) if "WORKER Luna" in l), finished_picture
ordered = render(flow, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation={"tab": "detail", "preview": {
    "target": ("result", "builder"), "report": {"rows": [{"outcome": "accepted", "calls": [{"answer": "DONE TEXT"}]}]}}})
assert "DONE TEXT" not in ordered and "Main decision: admitted" not in ordered, ordered
auto_nav = {"tab": "detail"}
auto_board = render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation=auto_nav)
assert "[Detail]" in auto_board and "MAIN Sol 6" in auto_board.splitlines()[auto_nav["box"]["y1"]], auto_board
roomy_nav = {"tab": "detail"}
roomy = render(flow, "codex", 100, 60, view="graph", main_attempt="flow-attempt", navigation=roomy_nav)
roomy_lines = roomy.splitlines()
detail_top = next(i for i, line in enumerate(roomy_lines) if "[Detail]" in line)
detail_end = next(i for i in range(detail_top + 1, len(roomy_lines)) if roomy_lines[i].startswith("╰"))
# The board fills the pane with pinned key hints and a dock that takes every row the tree leaves.
assert detail_end - detail_top - 1 >= dock_body_rows(60) and len(roomy_lines) == 60, roomy
assert not any(line.strip() for line in roomy_lines[detail_end + 1:-3]) or roomy_lines[detail_end + 1].strip(), roomy
closed_board = render(flow, "codex", 100, 26, view="graph", main_attempt="flow-attempt", navigation={"tab": "detail", "dismissed": True})
assert "Latest statement:" not in closed_board and "No call selected" in closed_board, closed_board
# A board too short for the full overview still keeps a feasible box, auto or dismissed, and its warning row above it.
for rows_high in (26, 28, 30, 34, 60, 90):
    kept = {}
    for label, nav in (("auto", {"tab": "detail"}), ("closed", {"tab": "detail", "dismissed": True})):
        shown = render(flow, "codex", 100, rows_high, view="graph", main_attempt="flow-attempt", navigation=nav)
        kept[label] = (nav["box"]["y1"], nav["box"]["y2"])
        lines_shown = shown.splitlines()
        assert nav["box"]["y2"] - nav["box"]["y1"] + 1 >= 4 and nav["box"]["y2"] == rows_high - 2, (rows_high, nav["box"], shown)
        assert ("MAIN Sol 6" in shown.splitlines()[nav["box"]["y1"]]) == (label == "auto") and ("No call selected" in shown) == (label == "closed"), shown
        assert "no known main" in shown, shown
    if rows_high < 28:
        assert kept["auto"] == kept["closed"], (rows_high, kept)
# A room with four recorded mains and one worker retains its dock body and warnings as calls release rows.
four = deepcopy(msg_report)
four["room"]["participants"].append({"participant": "m4", "role": "main", "joined": True, "model": "gpt-6-luna", "seq": 4,
                                     "provider": "codex", "attempt": "a4"})
four["attempts"]["active_recent"].append({"attempt_id": "a4", "state": "working", "panel": {
    "room_id": "r", "room_participant": "m4", "role": "main"}})
four["room"]["messages"] = [
    {"id": "p%d" % i, "sender": "m1", "recipient": "m2", "targets": ["m2"], "message_kind": "note",
     "ts": "2026-10-07T10:%02d:00Z" % i, "text": "pair %d" % i} for i in range(6)]
four["room"]["pairs"] = [{"sender": "m1", "recipient": "m2", "sent": 6, "pending": 0}]
for variant in ("plain", "unlinked"):
    for rows_high in (32, 60, 90):
        kept = set()
        for advisors in (0, 1, 3):
            for workers in (0, 1, 3):
                report = crowd(four, advisors, workers, parent="a1", prefix=variant)
                if variant == "unlinked":
                    report["room"]["participants"].append({"participant": "orphan", "role": "worker", "joined": True, "parent": "gone",
                                                           "model": "gpt-6-luna", "label": "orphan"})
                    report["attempts"]["active_recent"].append({"attempt_id": "orphan", "state": "working", "panel": {
                        "room_id": "r", "room_participant": "orphan", "role": "worker"}})
                for state in ("working", "done"):
                    for attempt in report["attempts"]["active_recent"]:
                        if attempt["attempt_id"].startswith(variant + "-"):
                            attempt["state"] = state
                    nav = {"selected": ("chat", "m1"), "dismissed": True}
                    text = render(report, "codex", 157, rows_high, view="graph", navigation=nav, main_attempt="a1")
                    assert len(text.splitlines()) == rows_high, text
                    assert ("no known main" in text) == (variant == "unlinked"), text
                    assert nav["box"]["y2"] - nav["box"]["y1"] - 1 >= min(dock_body_rows(rows_high), max(3, nav["dock_content_rows"])), nav
                    kept.add((nav["box"]["y1"], nav["box"]["y2"]))
        assert all(y2 == rows_high - 2 and y2 - y1 + 1 >= 5 for y1, y2 in kept), (variant, rows_high, kept)
# A short board keeps the compact main card and one row per call, so its box takes every row they leave;
# neither size keeps the previous box, and open-in-full follows the box this frame draws.
for rows_high in (19, 16):
    nav = {"tab": "detail", "preview": {"target": ("result", "builder"), "report": {}}, "box": {"y1": 1, "y2": 2}, "full_result": ("result", "builder")}
    shown = render(flow, "codex", 100, rows_high, view="graph", main_attempt="flow-attempt", navigation=nav)
    assert nav["surface"] == "graph" and nav["tab_strip_visible"] and len(shown.splitlines()) == rows_high, nav
    strip_y = next(h["y"] for h in nav["hits"] if h["action"] == ("tab", "detail"))
    assert shown.splitlines()[strip_y - 1].startswith("╭─ Log  [Detail]  Plan"), shown
    if nav.get("box"):
        assert nav["box"]["y2"] == rows_high - 2 and nav.get("full_result") == ("result", "builder"), nav
    else:
        assert not nav.get("full_result") and "f Full" not in shown.splitlines()[-1], nav
# Tabs and body actions follow the drawn dock through resize, including help and the tree fallback.
for managed in (False, True):
    for keys_help in (False, True):
        for initial_tab in ("detail", "plan", "debate", "messages"):
            nav = {"tab": initial_tab, "keys_help": keys_help}
            for columns, rows_high in ((76, 16), (76, 19), (100, 30), (76, 16), (50, 10)):
                shown = render(flow, "codex", columns, rows_high, view="graph", main_attempt="flow-attempt",
                               navigation=nav, managed=managed)
                strip = [h for h in nav["hits"] if h["action"] == ("tab", "detail")]
                box = nav.get("box")
                assert len(shown.splitlines()) <= rows_high and all(display_width(line) <= columns for line in shown.splitlines()), shown
                assert all(1 <= h["y"] <= len(shown.splitlines()) and 1 <= h["x1"] <= h["x2"] <= columns
                           for h in nav["hits"]), nav
                if not keys_help:
                    assert strip and (not box or box["y2"] > box["y1"]), (columns, rows_high, nav, shown)
                footer = "\n".join(shown.splitlines()[-5:] if keys_help else shown.splitlines()[-1:])
                if not box:
                    assert not any(hint in footer for hint in
                                   ("wheel Scroll", "↑↓ Message", "↑↓ Seat", "←→ Filter", "←→ Target")), shown
                    assert not tab_event(("scroll", 1, 1, 1), nav), nav
                if not strip:
                    assert "Tab Next tab" not in footer, shown
                    before_tab = nav["tab"]
                    assert not tab_event(("tab",), nav) and nav["tab"] == before_tab, nav
                else:
                    assert tab_event(("click", strip[0]["x1"], strip[0]["y"]), nav) and nav["tab"] == "detail", nav
                    assert tab_event(("tab",), nav) and nav["tab"] == "plan", nav
                    # Keep each tab's body hints when a full box fits.
                    if box and not keys_help:
                        nav["tab"] = initial_tab
                        full = render(flow, "codex", columns, rows_high, view="graph", main_attempt="flow-attempt",
                                      navigation=nav, managed=managed)
                        expected = {"plan": "wheel Scroll", "messages": "↑↓ Message" if nav.get("message_ids") else "←→ Filter"}.get(initial_tab)
                        assert not expected or not nav.get("tab_body_visible") or expected in full.splitlines()[-1], full
                nav["tab"] = initial_tab
# People read the board: call exits become words, machine status lines and markdown marks are dropped.
import room_view as graph_text
clocked = deepcopy(single_flow)
for m in clocked["room"]["participants"]:
    m["joined_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 12 * 60))
for attempt in clocked["attempts"]["active_recent"]:
    attempt["panel"]["effort"] = "high"
for glyphs in (False, True):
    for columns, rows in ((80, 40), (127, 50)):
        clocked_cards = graph_text.render_graph(clocked, columns, rows, unicode=glyphs, navigation={"dismissed": True})
        assert re.search(r"running\]  12m", clocked_cards), clocked_cards
        assert not re.search(r"[│|]\s+high\s+[│|]", clocked_cards), clocked_cards
        assert "high" not in clocked_cards and " for " not in clocked_cards, clocked_cards
clocked_tree = render(clocked, "codex", 100, 50, view="tree", navigation={})
assert re.search(r"\[⟳ running\]  12m", clocked_tree), clocked_tree
with patch.object(graph_text.time, "time", return_value=1800000000):
    for seconds, expected in ((30, "<1m"), (720, "12m"), (7200, "2h"), (172800, "2d")):
        ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(1800000000 - seconds))
        assert graph_text.elapsed(ts) == expected
assert graph_text.elapsed("invalid") == ""
assert graph_text.readable("Call exit=0; parent acceptance pending.\nstop-reason: provider=codex is_error=0\n**Verification:** `ok`", " ") \
    == "Finished. Verification: ok"
assert graph_text.readable("Call exit=2; parent acceptance pending.", " ") == "Failed (exit 2)."
assert graph_text.readable("## Decision\n\nFirst paragraph\n\n\n- next step", paragraphs=True) == "Decision\n\nFirst paragraph\n\n- next step"
talking = deepcopy(flow)
talking["room"]["pairs"] = [{"sender": "flow-main", "recipient": "flow-other", "sent": 2, "pending": 1}]
picture = render(talking, "codex", 110, 30, view="graph", main_attempt="flow-attempt", navigation={})
assert "Between mains" not in picture and "BETWEEN MAINS" not in picture, picture
tall = render(talking, "codex", 157, 65, view="graph", main_attempt="flow-attempt", navigation={})
assert "BETWEEN MAINS" not in tall and "Between mains" not in tall, tall
assert "Model use, last calls" not in tall and "Click any block" not in tall, tall
assert "..." not in tall and sum("week" in line for line in tall.splitlines()) <= 1, tall
import time as pair_clock
stamp = pair_clock.strftime("%Y-%m-%dT%H:%M:%SZ", pair_clock.gmtime())
# Board truth: stale status shows its age, a guard stop reads as one, calls in review are grouped.
def stamp_ago(hours):
    return pair_clock.strftime("%Y-%m-%dT%H:%M:%SZ", pair_clock.gmtime(pair_clock.time() - hours * 3600))
truth = deepcopy(flow)
truth["room"]["participants"][0]["label"] = "Native task not recorded"
# Stale means a main still working long after its declaration, not merely an old status.
truth["main_activity"] = {"flow-main": {"state": "busy", "attempt": "flow-attempt"}}
truth["room"]["statuses"] = {"flow-main": {"text": "Now: 방 범위 선언", "ts": stamp_ago(6), "seq": 1},
                             "flow-other": {"text": "fresh work", "ts": stamp_ago(0.1), "seq": 2}}
truth["room"]["messages"] = [{"id": "result-builder", "sender": "builder", "recipient": "flow-attempt", "message_kind": "handoff",
    "pending_for": ["flow-attempt"], "ts": stamp,
    "text": "Call exit=1; parent acceptance pending.\n## Output\n\nThe worker guard stopped this run: protected state changed (gitmeta)."}]
truth["room"]["call_results"] = {"builder": {"exit": 1, "ts": stamp, "seq": 5, "read": False}}
for attempt in truth["attempts"]["active_recent"]:
    attempt["state"] = {"builder": "failed", "researcher": "review", "explorer": "review"}.get(attempt["attempt_id"], attempt["state"])
    if attempt["attempt_id"] == "builder":
        attempt["reason_code"] = "worker_authority_violation"
for glyphs, dot, ending in ((True, "·", "→"), (False, "/", "->")):
    board = render(truth, "codex", 157, 40, view="graph", unicode=glyphs, main_attempt="flow-attempt", navigation={})
    assert "Now: 방 범위 선언" in board and "Now: Now:" not in board, board
    assert "(stale)" in board and "fresh work" not in board, board
    assert "guard stop" in board and "failed (exit 1)" not in board, board
    assert board.count("awaiting decision") >= 2, board
nonguard = deepcopy(truth)
nonguard["room"]["messages"][0]["text"] = "Call exit=1; parent acceptance pending."
nonguard["room"]["call_results"]["builder"]["artifact"] = ".oms/artifacts/failed-output.md"
next(a for a in nonguard["attempts"]["active_recent"] if a["attempt_id"] == "builder")["reason_code"] = None
assert "failed: exit 1" in render(nonguard, "codex", 157, 40, view="graph", main_attempt="flow-attempt", navigation={})
# Recent message rows keep a fixed text column for different sender lengths, including wide CJK names.
aligned_recent = deepcopy(truth)
aligned_recent["room"]["messages"] = [
    {"id": "recent-a", "sender": "flow-main", "recipient": "all", "message_kind": "note",
     "ts": stamp, "text": "TREE-A"},
    {"id": "recent-b", "sender": "漢字送り手", "recipient": "all", "message_kind": "note",
     "ts": stamp, "text": "TREE-B"},
]
recent_tree = render(aligned_recent, "codex", 80, 40, view="tree", navigation={})
recent_lines = [line for line in recent_tree.splitlines() if "TREE-" in line and "[" not in line]
assert len(recent_lines) == 2, recent_tree
recent_columns = [display_width(line[:line.index("TREE-")]) for line in recent_lines]
assert recent_columns == [29, 29], (recent_columns, recent_lines)
# The tree never doubles "Now:" and a main with no readings gets one usage line instead of a box.
tree_board = render(truth, "codex", 157, 40, view="tree", navigation={})
assert "Now: 방 범위 선언" in tree_board and "Now: Now:" not in tree_board, tree_board
assert "Model use, last 8 calls" not in tree_board and "USAGE" not in tree_board, tree_board
bare = deepcopy(truth)
bare.pop("operations")
bare.pop("provider_status", None)
bare_board = render(bare, "codex", 157, 40, view="tree", navigation={})
assert bare_board.splitlines()[1] == "Claude -- │ Codex --" and "USAGE" not in bare_board, bare_board

# The tree head bounds repository tasks to the five newest open tasks, then links to the full Plan tab.
task_board = deepcopy(truth)
task_board["room"]["repo_tasks"] = [
    {"id": "t-ready-activity", "title": "Ready activity", "state": "ready", "updated": "2026-10-10T00:00:00Z",
     "updated_at": "2026-10-10T03:30:00Z"},
    {"id": "t-done", "title": "Closed done", "state": "done", "updated": "2026-10-10T01:00:00Z"},
    {"id": "t-review", "title": "Recent review", "state": "review", "updated": "2026-10-10T04:00:00Z"},
    {"id": "t-blocked", "title": "Closed blocked", "state": "blocked", "updated": "2026-10-10T00:30:00Z"},
    {"id": "t-claimed", "title": "Recent claimed", "state": "claimed", "updated": "2026-10-10T05:00:00Z"},
    {"id": "t-landing", "title": "Recent landing", "state": "landing", "updated": "2026-10-10T05:30:00Z"},
    {"id": "t-running", "title": "Recent running", "state": "running", "updated": "2026-10-10T04:30:00Z"},
    {"id": "t-ready-new", "title": "Newest ready", "state": "ready", "updated": "2026-10-10T06:00:00Z"},
]
task_nav = {}
task_text = render(task_board, "codex", 157, 40, view="tree", navigation=task_nav)
task_hits = [hit["action"] for hit in task_nav["hits"] if hit["action"][0] == "task"]
assert task_hits == [("task", ident) for ident in ("t-ready-new", "t-landing", "t-claimed", "t-running", "t-review")], task_hits
assert "Recent landing" in task_text and "Closed done" not in task_text and "Closed blocked" not in task_text, task_text
assert "1 more open · 1 done · 1 blocked · Plan tab for all" in task_text, task_text
summary_line = next(i for i, line in enumerate(task_text.splitlines()) if "Plan tab for all" in line)
summary_hit = next(hit for hit in task_nav["hits"] if hit["action"] == ("tab", "plan") and hit["y"] == summary_line + 1)
assert choose(("click", summary_hit["x1"], summary_hit["y"]), task_nav) == ("tab", "plan")
assert tab_event(("click", summary_hit["x1"], summary_hit["y"]), task_nav)
assert task_nav["tab"] == "plan", task_nav
from dashboard_projection import fit, use_unicode
use_unicode(True)
assert fit("abcdefghij", 6) == "abcde…"
use_unicode(False)
assert fit("abcdefghij", 6) == "abc..."
use_unicode(True)
from panel_view import activity, box_row, clipped
from dashboard_projection import char_width, clean
sample = "ok ✔️ é 각"
assert display_width(sample) == 9 and char_width("\u1160") == char_width("\u11ff") == char_width("\u0301") == 0
assert clean(sample) == clipped(sample, 20) == "ok ✔ é 각"
for box_width in (8, 14, 24):
    assert display_width(box_row(sample, box_width, True)) == box_width
assert display_width(fit(sample + " long tail", 8)) <= 8
assert activity("waiting_approval") == "!" and activity("waiting_approval", unicode=False) == "!" and activity("waiting_input") == "?"
# A failed call marks its own main's window with "! "; a handled failure or another main's call does not.
alarm = deepcopy(flow)
assert not panel.main_needs_attention(alarm, "flow-attempt")
next(a for a in alarm["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = "failed"
assert panel.main_needs_attention(alarm, "flow-attempt") and not panel.main_needs_attention(alarm, "flow-other")
alarm["room"]["messages"] = [{"id": "result-builder", "sender": "builder", "recipient": "flow-attempt", "message_kind": "handoff", "pending_for": []}]
assert not panel.main_needs_attention(alarm, "flow-attempt")
# A handled failure stays handled once later mail evicts its handoff from the message window.
from panel_view import inbox_items
assert not any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
alarm["room"]["messages"] = []
assert panel.main_needs_attention(alarm, "flow-attempt")
assert any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
alarm["room"]["call_results"] = {"builder": {"exit": 1, "read": True}}
assert not panel.main_needs_attention(alarm, "flow-attempt")
assert not any(i["action"] == ("result", "builder") for i in inbox_items(alarm))
for state in ("failed", "timed_out", "abandoned", "orphaned", "review", "blocked", "waiting_approval"):
    failure = deepcopy(alarm)
    next(a for a in failure["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = state
    assert any(i["action"] == ("result", "builder") for i in inbox_items(failure)) == (state in {"review", "waiting_approval"}), state
    member = next(m for m in graph_text.nodes(failure) if m["participant"] == "builder")
    assert (graph_text.call_classifier(failure)(member) == "past") == (state not in {"review", "blocked", "waiting_approval"}), state
# Context headers cover main variants, reader composition, escaping, and border restoration.
header_state = {"room": {"id": "fixture", "participants": [
    {"participant": "header-main", "role": "main", "provider": "claude", "harness": "claude",
     "joined": True, "model": "Opus 5.5"},
    {"participant": "header-call", "role": "worker", "provider": "codex", "joined": True,
     "model": "test-model", "label": "Build result"}]},
    "attempts": {"active_recent": [], "recent": []}, "repo": {"name": "repo", "branch": "feature#one"}}
for harness, shown in (("claude", "Claude Code"), ("codex", "Codex"), ("pi", "Pi")):
    header_state["room"]["participants"][0]["harness"] = harness
    header_state["room"]["participants"][0]["provider"] = harness
    label = panel.context_header_text(header_state, "header-main", Path("repo"))
    assert shown in label and "task unrecorded" in label and "feature##one" in label, label
header_state["room"]["participants"][0]["harness"] = "claude"
header_state["room"]["participants"][0]["provider"] = "claude"
header_state["provider_status"] = {"claude": {"participant": "header-main", "context_left": 39}}
assert "ctx 61% used" in panel.context_header_text(header_state, "header-main", Path("repo"))
assert "unknown" in panel.context_header_text({"repo": {}}, "absent", Path("repo"))

def header_window_fixture(local_values, reader=False, fail_capture=False,
                          reader_target=("result", "header-call")):
    options = {"@oms_panel_native_pane": "%8"}
    options.update(local_values)
    calls = []
    def fake_tmux(argv, **kwargs):
        args = argv[1:]
        calls.append(args)
        if args[0] == "show-options":
            option = args[-1]
            if fail_capture and "-v" in args and option == "pane-border-format":
                return subprocess.CompletedProcess(argv, 1, "", "capture failed")
            value = local_values.get(option)
            if value is None:
                output = ""
            elif "-v" in args:
                output = value + "\n"
            else:
                quoted = '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'
                output = option + " " + quoted + "\n"
            return subprocess.CompletedProcess(argv, 0, output, "")
        if args[0] == "show-option":
            value = options.get(args[-1])
            return subprocess.CompletedProcess(argv, 0 if value is not None else 1, (value or "") + "\n", "")
        if args[0] == "display-message":
            return subprocess.CompletedProcess(argv, 0, "80\n", "")
        if args[0] == "set-option":
            if "-u" in args:
                options.pop(args[-1], None)
            else:
                options[args[-2]] = args[-1]
        return subprocess.CompletedProcess(argv, 0, "", "")
    os.environ.update(TMUX="fixture", TMUX_PANE="%7", OMS_PANEL_SESSION="oms-panel-0123456789ab")
    with patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel.subprocess, "run", side_effect=fake_tmux), \
            patch.object(panel.panel_reader, "find", return_value="%9" if reader else None):
        navigation = {"tab": "detail", "preview": {"target": ("result", "header-main")},
                      "reader_target": reader_target}
        panel.update_context_header(header_state, "header-main", Path("repo"), navigation)
        panel.update_context_header(header_state, "header-main", Path("repo"), navigation)
        panel.toggle_context_headers()
        for option in ("pane-border-status", "pane-border-format"):
            assert options.get(option) == local_values.get(option), (option, options, local_values)
            assert (option in options) == (option in local_values), (option, options, local_values)
        if reader:
            reader_headers = [args[-1] for args in calls if args[0] == "set-option" and
                              args[-2] == panel.HEADER_OPTION and args[-3] == "%9"]
            assert reader_headers and all(value == "" for value in reader_headers), calls
        if not local_values:
            unset = {args[-1] for args in calls if args[0] == "set-option" and "-u" in args}
            assert {"pane-border-status", "pane-border-format"} <= unset, calls
        if fail_capture:
            changed_borders = [args for args in calls if args[0] == "set-option" and
                               args[-2] in ("pane-border-status", "pane-border-format")]
            assert not changed_borders, changed_borders
            assert panel.HEADER_ENABLED not in options, options
    return calls, options

header_window_fixture({}, reader=True)
header_window_fixture({}, reader=True, reader_target=("debate", ("fixture-task", "header-call")))
quoted_format = "format with spaces, #, 'single' and \"double\" quotes"
header_window_fixture({"pane-border-status": "bottom", "pane-border-format": quoted_format})
header_window_fixture({"pane-border-status": "bottom", "pane-border-format": quoted_format},
                      fail_capture=True)
assert panel._tmux_output_value("value\n\n") == "value\n"
for code in (1, -9):
    with patch.object(panel, "tmux_command", side_effect=lambda *args: ["fixture-tmux", *args]), \
            patch.object(panel.subprocess, "run",
                         return_value=subprocess.CompletedProcess([], code, "", "")):
        assert panel._window_option_local("%7", "pane-border-format") == (None, ""), code

# An attention-marked window name keeps the geometry fields and the split text apart.
with patch.object(panel, "tmux_command", side_effect=lambda *a: ["x"]), patch.object(panel.subprocess, "run",
        return_value=subprocess.CompletedProcess([], 0, "1 0 2 100 50 100 25\t! codex\ttop:30\n", "")):
    assert panel.panel_geometry() == (True, False, 2, (100, 50), (100, 25), "top:30", "work", "", "")
for headers in ("off", "top", "bottom"):
    pane_height = 49 if headers != "off" else 50
    with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0,
            "1 0 2 100 50 25 %s %s\t! codex\tleft:25c\twork\t%%2\t\n" % (pane_height, headers), "")):
        geometry = panel.panel_geometry()
    assert geometry == (True, False, 2, (100, 50), (25, 50), "left:25c", "work", "%2", ""), geometry
    done, calls = sync_run(panel.SplitSync(), geometry)
    assert done == [None] and calls == [], (headers, done, calls)
renames = []
class TmuxStub:
    def __init__(self, name):
        self.name = name
    def __call__(self, command, **kwargs):
        if "rename-window" in command:
            renames.append(command[-1])
            self.name = command[-1]
        return subprocess.CompletedProcess(command, 0, self.name + "\n" if "display-message" in command else "")
os.environ.update(TMUX="x", TMUX_PANE="%7", OMS_PANEL_SESSION="oms-panel-0123456789ab")
for original, marks in (("codex", ["! codex", "codex"]), ("my-shell", [])):
    with patch.object(panel.subprocess, "run", TmuxStub(original)):
        panel.mark_window(True)
        panel.mark_window(True)
        panel.mark_window(False)
    assert renames == marks, (original, renames)
    renames.clear()
with patch.object(panel.subprocess, "run", TmuxStub("codex")):
    for flags in ((False, True), (True, True), (False, False)):
        panel.mark_window(*flags)
assert renames == ["? codex", "! codex", "codex"], renames
renames.clear()
for name in ("TMUX", "TMUX_PANE", "OMS_PANEL_SESSION"):
    del os.environ[name]
# A short board shows a notice instead of dropping it.
assert "Opening chat" in render(flow, "codex", 100, 17, view="graph", main_attempt="flow-attempt",
                                navigation={"notice": "Opening chat..."})
# Messages and details name each participant with its role, so a Sol worker never reads like the Sol main.
same_model = deepcopy(flow)
same_model["room"]["participants"].append({"participant": "sol-worker", "role": "worker", "model": "gpt-6-sol",
                                           "joined": True, "parent": "flow-attempt", "label": "Same model worker"})
same_model["room"]["messages"] = [{"sender": "sol-worker", "targets": ["flow-main"], "text": "Call exit=1; parent acceptance pending."}]
picture = render(same_model, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                 navigation={"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}, "band_offsets": {"detail": 99}})
assert "From Sol 6 worker:" not in picture and "Mail" in picture, picture
sent_board = render(same_model, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                    navigation={"dismissed": True, "tab": "messages"})
assert "WORKER Sol 6 · Same model worker → MAIN Sol 6 · Main native chat · Failed (exit 1)." in sent_board, sent_board
# A finished call stays reachable through the Past tab after leaving the active tree.
done_flow = deepcopy(flow)
for attempt in done_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "builder":
        attempt["state"] = "done"
team_nav = {"tab": "past", "preview": {"target": ("chat", "flow-main"), "report": {}}, "band_offsets": {"detail": 1}}
render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt", navigation=team_nav)
row = next(h for h in team_nav["hits"] if h["action"] == ("result", "builder"))
assert tab_event(("click", row["x1"], row["y"]), team_nav) and team_nav["preview"] == {"target": ("result", "builder")}
assert "WORKER Sonnet 5.5" in render(done_flow, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                                                  navigation=team_nav)
# Expanded detail puts decisions before raw answers and keeps every grouped team row actionable.
people = graph_text.nodes(done_flow)
main = next(m for m in people if m["participant"] == "flow-main")
team = [m for m in people if m["participant"] in {"advisor", "researcher", "builder"}]
team[0]["state"] = "waiting_approval"
team[1]["state"] = "working"
team[2]["state"] = "done"
team[2]["title"] = "한글 " * 40 + "DETAIL END"
team.append(dict(team[2], participant="unknown-call", state="presence unknown", title="Unknown call"))
for columns in (28, 76, 110):
    lines, targets = graph_text.detail(main, {}, columns, people,
        {"messages": [{"sender": "advisor", "targets": ["flow-main"], "text": "message " * 60 + "MESSAGE END"}]},
        everyone={"flow-main": team})
    assert lines.index("  Needs attention") < lines.index("  Active") < lines.index("  Unrecorded") < lines.index("  Finished"), lines
    assert set(targets.values()) == {("result", m["participant"]) for m in team}, targets
    assert next(i for i,line in enumerate(lines) if "Messages to" in line) > lines.index("  Finished"), lines
    for row, target in targets.items():
        assert 0 <= row < len(lines) and target[1] in {m["participant"] for m in team}
        assert display_width(lines[row].lstrip()) <= columns - 4, (columns, lines[row])
        if row - 1 in targets and targets[row] == targets[row - 1]:
            assert lines[row].startswith("    "), (columns, lines[row])
    finished_text = " ".join(lines[row].strip() for row in targets if targets[row] == ("result", team[2]["participant"]))
    assert finished_text.endswith("DETAIL END"), finished_text
    message_start = next(i for i, line in enumerate(lines) if line.startswith("Messages to ")) + 1
    message_preview = []
    for line in lines[message_start:]:
        if not line.startswith(("  From ", "    ")):
            break
        message_preview.append(line)
    assert len(message_preview) <= 2 and message_preview[-1].rstrip().endswith("..."), message_preview
    message_row = next(i for i, line in enumerate(lines) if line.startswith("  From "))
    for row in range(message_row, lines.index("", message_row)):
        assert display_width(lines[row].lstrip()) <= columns - 4, (columns, lines[row])
        assert lines[row].startswith("  " if row == message_row else "    "), (columns, lines[row])
    assert any("Team:" in line for line in lines) and any("Messages to" in line for line in lines)
assert all(line.startswith("    ") for index, line in enumerate(lines) if index in targets and index - 1 in targets and targets[index] == targets[index - 1]), lines
seven_done = [dict(team[2], participant="done-%s" % n, title="Done %s" % n,
                   ended="2026-10-10T10:%02d:00Z" % n, seq=7 - n) for n in range(7)]
seven_lines, seven_targets = graph_text.detail(main, {}, 100, people, {}, everyone={"flow-main": seven_done})
assert len(seven_targets) == 5 and list(seven_targets.values()) == [("result", "done-%s" % n) for n in range(6, 1, -1)]
assert "2 earlier finished · see the Past tab" in seven_lines and seven_lines.index("2 earlier finished · see the Past tab") not in seven_targets
mail_room = {"participants": [dict(team[2], parent="flow-main")],
             "messages": [{"id": "result-builder", "sender": "builder", "recipient": "flow-main",
                           "targets": ["flow-main"], "message_kind": "handoff", "text": "INTERNAL HANDOFF"},
                          {"sender": "advisor", "targets": ["flow-main"], "text": "DIRECT MESSAGE"}],
             "pairs": [{"recipient": "flow-main", "pending": 4}]}
mail_detail = graph_text.detail(main, {}, 100, people, mail_room, everyone={"flow-main": team})[0]
assert "INTERNAL HANDOFF" not in "\n".join(mail_detail) and "DIRECT MESSAGE" in "\n".join(mail_detail), mail_detail
assert "Messages to Sol 6 main: 1 (4 unread)" in mail_detail, mail_detail
structured_message = dict(mail_room, messages=[{"sender": "advisor", "targets": ["flow-main"],
    "text": "Summary\n\n| size | issue |\n|------|-------|\n| large | joined |\n\n```bash\noms panel | size\n```"}])
structured_detail = graph_text.detail(main, {}, 80, people, structured_message, everyone={"flow-main": team})[0]
structured_start = next(i for i, line in enumerate(structured_detail) if line.startswith("Messages to ")) + 1
structured_preview = []
for line in structured_detail[structured_start:]:
    if not line.startswith(("  From ", "    ")):
        break
    structured_preview.append(line)
assert len(structured_preview) == 2 and "| size | issue |" in structured_preview[1] and structured_preview[1].endswith("..."), structured_preview
# Main mail readers retain table/fence layout, read state and room isolation; empty cards offer no f action.
mail_flow = deepcopy(done_flow)
mail_body = "\n  **Summary** snake__case `literal`  \nmodel-result: actual peer finding\n\n| size | issue |\n|------|-------|\n| large | joined |\n\n```bash\n  oms panel | size\n\tprintf '\\n'\n```\nLAST MAIL LINE\n\n"
mail_flow["room"].update(closed=False, messages=[
    {"sender": "advisor", "targets": ["flow-main"], "ts": "2026-10-10T09:00:00Z", "text": "EARLIER MAIL"},
    {"sender": "advisor", "targets": ["flow-other"], "text": "PRIVATE OTHER MAIL"},
    {"sender": "advisor", "targets": ["flow-main"], "pending_for": ["flow-main"],
     "ts": "2026-10-10T10:00:00Z", "text": mail_body}])
mail_frozen = json.dumps(mail_flow, sort_keys=True)
for surface in ("graph", "tree"):
    for automatic, managed in ((False, False), (False, True), (True, False), (True, True)):
        mail_nav = {"tab": "detail"} if automatic else {
            "tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
        mail_screen = render(mail_flow, "codex", 120, 44, view=surface, main_attempt="flow-attempt",
                             navigation=mail_nav, managed=managed, unicode=False)
        assert mail_nav["full_mail"] == ("mail", "flow-main"), mail_nav
        assert ("f Open right" if managed else "f Full text") in mail_screen, mail_screen
        assert managed or "f Open right" not in mail_screen, mail_screen
        assert mail_screen.splitlines()[-1].isascii(), mail_screen
        help_nav = dict(mail_nav, keys_help=True, help_rows=8)
        help_text = "\n".join(graph_text.footer_hints(help_nav, 120, managed, False, main, True))
        assert ("f Open right" if managed else "f Full text") in help_text, help_text
        assert "LAST MAIL LINE" not in mail_screen and choose(("f",), mail_nav) == ("mail", "flow-main")
        assert choose(("f",), dict(mail_nav, tab_body_visible=False)) is None
        mail_nav["tab"] = "plan"
        render(mail_flow, "codex", 120, 44, view=surface, navigation=mail_nav, unicode=False)
        assert not mail_nav.get("full_mail") and choose(("f",), mail_nav) is None
    empty_mail = deepcopy(mail_flow)
    empty_mail["room"]["messages"] = [mail_flow["room"]["messages"][1]]
    mail_nav = {"tab": "detail", "full_mail": ("mail", "flow-main"),
                "tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
    empty_screen = render(empty_mail, "codex", 120, 44, view=surface, navigation=mail_nav, managed=True, unicode=False)
    assert not mail_nav.get("full_mail") and "f Open right" not in empty_screen and choose(("f",), mail_nav) is None
mail_page = panel.panel_messages.mail_page(mail_flow["room"], "flow-main", 120)
readable_mail = mail_body
assert readable_mail in mail_page and "PRIVATE OTHER MAIL" not in mail_page, mail_page
assert "snakecase" in panel.panel_messages.readable_text(mail_body)
assert "model-result: actual peer finding" not in panel.panel_messages.readable_text(mail_body)
assert "| size | issue |\n|------|-------|\n| large | joined |" in mail_page
assert "```bash\n  oms panel | size\n\tprintf '\\n'\n```" in mail_page
assert mail_page.index("EARLIER MAIL") < mail_page.index(readable_mail) and mail_page.count("From ") == 2
assert panel.panel_messages.stamp("2026-10-10T10:00:00Z") in mail_page
long_mail = deepcopy(mail_flow)
long_mail["room"]["messages"][-1]["text"] = "```\n" + "x" * 200 + "\n```\nRETAINED END"
narrow_mail = panel.panel_messages.mail_page(long_mail["room"], "flow-main", 24)
assert "RETAINED END" in narrow_mail and "↪" not in narrow_mail
assert all(display_width(line) <= 24 for line in narrow_mail.splitlines()), narrow_mail
hostile_mail = deepcopy(mail_flow)
hostile_mail["room"]["messages"][-1]["text"] = "```bash\n  printf '\x1b[2J'\x00\n```\n\x1b[31mEND\u202e\u200b\r\x07"
inert_page = panel.panel_messages.mail_page(hostile_mail["room"], "flow-main", 120)
assert "\x1b" not in inert_page and "\x00" not in inert_page and "```bash\n  printf '?[2J'?\n```" in inert_page, inert_page
assert "?[31mEND????" in inert_page and not any(c in inert_page for c in "\u202e\u200b\r\x07"), inert_page
with patch.object(panel, "repository", return_value=project), \
        patch.object(panel, "work_repository", return_value=project), \
        patch.object(panel, "native_repository", return_value=project), \
        patch.object(panel.room, "status", return_value=mail_flow["room"]), \
        patch.object(panel.room, "messages", return_value=mail_flow["room"]["messages"]) as full_log, \
        patch.object(panel.room, "acknowledge", side_effect=AssertionError("reader acknowledged mail")), \
        patch.object(panel.room, "send", side_effect=AssertionError("reader wrote mail")), \
        patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((120, 44))), \
        patch.dict(os.environ, {panel_reader.BOARD: "%1"}, clear=True), \
        patch.object(panel.sys, "stdin") as reader_stdin, patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.panel_reader, "page", side_effect=lambda render_page, board: (
            0 if board == "%1" and readable_mail in render_page(120) else 1)) as paged:
    reader_stdin.isatty.return_value = True
    with patch.object(panel.sys.stdout, "isatty", return_value=True):
        assert panel.main(["--repo", str(project), "--room", "flow-room", "--mail", "flow-main"]) == 0
    paged.assert_called_once()
    full_log.assert_called_once_with(project, "flow-room")
assert json.dumps(mail_flow, sort_keys=True) == mail_frozen, "mail reader changed room state"
# A note older than the status window remains available and both readers fetch its complete body.
mail_records = [{"thread": "flow-room", "seq": 0, "room_event": {"kind": "created"}}]
mail_records += [{"seq": i + 1, "room_event": dict(kind="join", participant=m["participant"],
    provider=m.get("provider", "codex"), role=m["role"], label=m["label"], model=m.get("model", ""))}
    for i, m in enumerate(mail_flow["room"]["participants"])]
mail_start = len(mail_records)
mail_records += [{"seq": mail_start, "text": "OLDER NOTE\n" + mail_body, "room_event": {
    "kind": "message", "id": "old-mail", "sender": "advisor", "recipient": "flow-main", "message_kind": "note"}}]
mail_records += [{"seq": mail_start + i + 1, "text": "New status", "room_event": {
    "kind": "message", "id": "mail-status-%s" % i, "sender": "advisor", "recipient": "flow-other",
    "message_kind": "status"}} for i in range(12)]
with patch.object(panel.room, "records", return_value=mail_records), \
        patch.object(panel.room, "acknowledge", side_effect=AssertionError("reader acknowledged old mail")), \
        patch.object(panel.room, "send", side_effect=AssertionError("reader wrote old mail")):
    old_flow = dict(mail_flow, room=panel.room.status(project, "flow-room"))
    assert len(old_flow["room"]["messages"]) == 12
    assert not panel.panel_messages.main_mail(old_flow["room"], "flow-main")
    old_frozen = json.dumps(old_flow, sort_keys=True)
    for projection in (old_flow, dict(old_flow, room=dict(old_flow["room"], pairs=[]))):
        old_nav = {"tab": "detail", "selected": ("chat", "flow-main")}
        old_screen = render(projection, "codex", 157, 44, view="graph", main_attempt="flow-attempt",
                            navigation=old_nav, unicode=False)
        assert old_nav["full_mail"] == ("mail", "flow-main") and choose(("f",), old_nav) == ("mail", "flow-main")
        assert "Tab Next tab" in old_screen.splitlines()[-1] and "q Quit" in old_screen.splitlines()[-1]
    # Pair totals include result handoffs, so neither read mail beyond the window nor a handoff alone enables f;
    # the Messages tab still lists read mail.
    for projection in (dict(old_flow, room=dict(old_flow["room"], pending_messages=[])),
                       dict(old_flow, room=dict(old_flow["room"], messages=[], pending_messages=[],
                            pairs=[{"recipient": "flow-main", "sent": 1, "pending": 1}]))):
        none_nav = {"tab": "detail", "selected": ("chat", "flow-main")}
        render(projection, "codex", 157, 44, view="graph", main_attempt="flow-attempt", navigation=none_nav, unicode=False)
        assert not none_nav.get("full_mail") and choose(("f",), none_nav) is None, none_nav
    with patch.object(panel, "managed_session", return_value=False):
        panel.navigate(project, ("mail", "flow-main"), old_flow, old_nav, 120)
    assert "OLDER NOTE\n" + mail_body in old_nav["detail"]["text"], old_nav["detail"]
    with patch.object(panel, "repository", return_value=project), \
            patch.object(panel, "work_repository", return_value=project), \
            patch.object(panel, "native_repository", return_value=project), \
            patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((120, 44))), \
            patch.dict(os.environ, {}, clear=True), patch.object(panel.sys, "stdout", io.StringIO()) as mail_stdout:
        assert panel.main(["--repo", str(project), "--room", "flow-room", "--mail", "flow-main"]) == 0
        assert "OLDER NOTE\n" + mail_body in mail_stdout.getvalue(), mail_stdout.getvalue()
    assert json.dumps(old_flow, sort_keys=True) == old_frozen
    bounded_log = panel.room.messages(project, "flow-room") + [
        dict(old_flow["room"]["messages"][-1], text="Bounded status") for _ in range(panel.panel_messages.MAX_MESSAGES)]
    with patch.object(panel.room, "messages", return_value=bounded_log):
        assert panel.panel_messages.read(project, "flow-room") == bounded_log[-panel.panel_messages.MAX_MESSAGES:]
        panel.navigate(project, ("mail", "flow-main"), old_flow, old_nav, 120)
        assert "OLDER NOTE" not in old_nav["detail"]["text"]
with patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "read_panel", return_value=(mail_flow, 0, "codex", "flow-attempt", "graph", False, "flow-room")), \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "terminal_style", return_value=(True, False, False)), \
        patch.object(panel, "provider_availability", return_value={}), \
        patch.object(panel, "main_installed", return_value=False), patch.object(panel, "mark_window"), \
        patch.object(panel, "reload_ready", return_value=False), patch.object(panel, "session_option", return_value=None), \
        patch.object(panel, "panel_geometry", return_value=None), \
        patch.object(panel.panel_host, "herdr_active", return_value=False), \
        patch.object(panel.panel_reader, "open_reader", return_value="%9") as opened, \
        patch.object(panel.panel_reader, "close_reader", return_value=False), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((120, 44))), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.dict(os.environ, {"TMUX_PANE": "%1"}, clear=True), \
        patch("panel_input.TerminalInput", return_value=BatchInput([[("tab",)], [("f",)], [("eof",)]])):
    assert panel.watch(project, "codex", view="graph", no_animation=True) == 0
opened.assert_called_once()
assert opened.call_args.args[0] == "%1" and shlex.split(opened.call_args.args[1])[-4:] == [
    "--mail", "flow-main", "--room", "flow-room"], opened.call_args
# End main mail reader assertions.
assert not any(line.startswith("Scope:") for line in lines)
scoped_main = graph_text.detail(dict(main, owns=["scripts/lib", "docs"]), {}, 76, people, {}, everyone={"flow-main": team})[0]
assert "Scope: scripts/lib, docs" in scoped_main, scoped_main
unknown_call = dict(team[2], state="presence unknown")
lines, targets = graph_text.detail(main, {}, 76, people, {}, everyone={"flow-main": [unknown_call]})
unknown_detail = " ".join(graph_text.detail(unknown_call, {"report": {"rows": []}}, 76, people, {})[0])
assert "status unknown" in unknown_detail and "Status unknown: no live process, heartbeat or exit seen" in unknown_detail \
    and "f opens it" in unknown_detail, unknown_detail
# The Detail scrolls, so narrow panels wrap the whole explanation, including what to do.
for columns in (24, 38):
    narrow_unknown = graph_text.detail(unknown_call, {"report": {"rows": []}}, columns, people, {})[0]
    assert "kept until its main decides. Any recorded result is below; f opens it" in " ".join(narrow_unknown), narrow_unknown
    narrow_main = graph_text.detail(main, {}, columns, people, {}, everyone={"flow-main": [unknown_call]})[0]
    assert "not a failure, kept until its main decides" in " ".join(line.strip() for line in narrow_main), narrow_main
assert "Team: 1 unrecorded" in lines and "  Active" not in lines and "  Finished" not in lines
assert any(line.startswith("    no live process, heartbeat or exit seen; not a failure")
           for line in lines[lines.index("  Unrecorded"):lines.index("Messages to Sol 6 main: 0")]), lines
assert any("Messages to" in line for line in lines) and ("result", "builder") in targets.values()
worker = next(m for m in people if m["participant"] == "builder")
recorded = {"rows": [{"outcome": "accepted", "verification": {"status": "passed"},
    "summary": "OWNER DECISION", "calls": [{"answer": "RAW WORKER ANSWER"}]}]}
kept = json.dumps(recorded, sort_keys=True)
lines, targets = graph_text.detail(worker, {"report": recorded}, 76, people)
assert lines.index("Main's summary:") < lines.index("Worker's answer:"), lines
assert "  OWNER DECISION" in lines and "  RAW WORKER ANSWER" in lines
assert "Accepted by its main · checks passed" in lines
assert any("Permissions unrecorded" in line for line in lines) and not any("Read-only" in line for line in lines)
# A running call never borrows a whole-main result its main recorded for another task.
adopted = {"rows": [{"task_id": "main-wide", "outcome": "completed", "summary": "OTHER TASK SUMMARY", "calls": []},
                    {"task_id": "own-task", "outcome": None, "summary": None, "calls": []}]}
lines, targets = graph_text.detail(dict(worker, task_id="own-task", state="working"), {"report": adopted}, 76, people)
assert not any("OTHER TASK SUMMARY" in line for line in lines), lines
assert "Not reviewed by its main yet · no checks run" in lines, lines
long_recorded = deepcopy(recorded)
long_recorded["rows"][0]["summary"] = "\n".join("decision line %s" % i for i in range(1000))
long_recorded["rows"][0]["calls"][0]["answer"] = "answer " * 20000
lines, _ = graph_text.detail(worker, {"report": long_recorded}, 76, people)
assert len(lines) < 60 and sum("More in the full recorded result" in line for line in lines) == 2, lines
assert json.dumps(recorded, sort_keys=True) == kept
# f opens the full retained reader through the existing revalidated result path; Enter stays a preview.
assert TerminalInput().decode(b"f") == [("f",)]
full_recorded = deepcopy(recorded)
full_recorded["rows"][0]["calls"][0]["answer"] = "\n".join("full line %s" % i for i in range(80)) + "\nFINAL RETAINED LINE"
full_nav = {"selected": ("result", "builder"), "tab": "detail", "preview": {"target": ("result", "builder"), "report": full_recorded}}
preview_screen = render(done_flow, "codex", 110, 40, view="graph", navigation=full_nav, managed=True)
assert "FINAL RETAINED LINE" not in preview_screen and "Enter Open right" in preview_screen, preview_screen
full_action = choose(("f",), full_nav)
assert full_action == ("result", "builder"), full_nav
for surface in ("graph", "tree"):
    debate_target = ("debate", ("task.1", "flow-main"))
    assert choose(("f",), {"surface": surface, "items": [debate_target], "full_result": debate_target,
                           "preview": {"target": debate_target}}) == debate_target
tree_full_nav = {"selected": ("result", "builder"),
                 "tab": "detail", "preview": {"target": ("result", "builder"), "report": full_recorded}}
tree_preview = render(done_flow, "codex", 110, 40, view="tree", navigation=tree_full_nav)
assert tree_full_nav["tab_body_visible"] and "f Full result" in tree_preview, tree_preview
assert "FINAL RETAINED LINE" not in tree_preview, tree_preview
assert choose(("f",), tree_full_nav) == ("result", "builder"), tree_full_nav
assert choose(("enter",), full_nav) is None and full_nav["preview"]["report"] is full_recorded
same_hit = {"y": 1, "x1": 1, "x2": 9, "action": ("result", "builder"), "preview": True}
same_nav = dict(full_nav, hits=[same_hit])
assert choose(("click", 2, 1), same_nav) is None and same_nav["preview"]["report"] is full_recorded
assert choose(("click", 2, 1), dict(same_nav, hits=[dict(same_hit, action=("result", "other"))],
                                    items=[("result", "other")])) is None
with patch.object(panel.room, "status", return_value=done_flow["room"]), \
        patch.object(panel, "call_results", return_value=deepcopy(full_recorded)) as fetch:
    # Opening is asynchronous: navigate only asks, the watcher's background read completes it.
    panel.navigate(project, full_action, done_flow, full_nav, 110)
    assert full_nav["opening"] == ("result", "builder") and not fetch.called, full_nav
    arrived = panel.read_evidence(project, full_nav["opening"])
    fetch.assert_called_once_with(project, ("result", "builder"))
    late = dict(full_nav, opening=None)
    assert not panel.finish_opening(late, arrived) and "detail" not in late, "a stale read reopened a closed detail"
    assert panel.finish_opening(full_nav, arrived) and "opening" not in full_nav
assert "FINAL RETAINED LINE" in full_nav["detail"]["report"]["rows"][0]["calls"][0]["answer"]
for unused in range(12):
    full_screen = render_detail(done_flow, full_nav["detail"], 110, 24, full_nav)
    if "FINAL RETAINED LINE" in full_screen:
        break
    choose(("pagedown",), full_nav)
else:
    raise AssertionError("full reader could not reach the retained last line")
# A one-row preview scrolls the same body index used by its title and click mapping; a further call row is what
# leaves short boards a one-row box.
def with_calls(report, count):
    report = deepcopy(report)
    for n in range(count):
        report["room"]["participants"].append({"participant": "row-%s" % n, "role": "worker", "model": "gpt-6-luna",
                                               "joined": True, "parent": "flow-attempt", "label": "ROW %s" % n})
        report["attempts"]["active_recent"].append({"attempt_id": "row-%s" % n, "state": "working", "panel": {
            "room_id": "flow-room", "room_participant": "row-%s" % n, "role": "worker"}})
    return report


short_detail_cases = []
short_flows = (done_flow, with_calls(done_flow, 1))
for columns, rows, short_flow in [(c, r, f) for c in (76, 100) for r in range(16, 28) for f in short_flows]:
    nav = {"tab": "detail", "preview": {"target": ("result", "builder"), "report": full_recorded}, "band_offsets": {"detail": 5}}
    screen = render(short_flow, "codex", columns, rows, view="graph", navigation=nav)
    band = next((b for b in nav.get("bands", []) if b["name"] == "detail"), None)
    if band and band["y2"] - band["y1"] == 2:
        body, _ = graph_text.detail(worker, {"report": full_recorded}, columns - 4, people)
        first = nav["band_offsets"]["detail"]
        shown_worker = next(m for m in graph_text.nodes(short_flow) if m["participant"] == "builder")
        summary_body, _ = graph_text.summary_card(shown_worker, {"report": full_recorded}, columns - 4, report=short_flow)
        assert summary_body[first][:20].strip() in screen.splitlines()[band["y1"]], (columns, rows, first, screen)
        hit = next(h for h in nav["hits"] if h["y"] == band["y1"] + 1)
        assert hit["action"] == ("result", "builder"), hit
        short_detail_cases.append((columns, rows))
assert short_detail_cases, "one-row summary fixture was not exercised"
assert choose(("f",), {"surface": "graph", "items": [("result", "builder")], "preview": {"target": ("chat", "flow-main")},
    "full_result": ("result", "builder")}) is None, "a stale preview opened a result"
long_dock_recorded = deepcopy(long_recorded)
long_dock_recorded["rows"][0].update(stages={"implement": {"state": "passed"}, "verify": {"state": "passed"},
                                             "review": {"state": "not_run"}, "main_decision": {"state": "pending"}},
                                      next_action="record main decision")
long_dock_recorded["rows"][0]["calls"][0].update(attempt_id="builder", artifact=".oms/artifacts/delegate/" + "a" * 150 + ".md",
                                               patch=".oms/artifacts/delegate/" + "p" * 150 + ".patch")
long_dock_flow = deepcopy(done_flow)
next(m for m in long_dock_flow["room"]["participants"] if m["participant"] == "builder")["joined_at"] = "2026-10-14T00:00:00Z"
for columns, rows in ((76, 24), (100, 30), (160, 42)):
    for ascii_only in (False, True):
        nav = {"tab": "detail", "preview": {"target": ("result", "builder"), "report": long_dock_recorded}}
        screen = render(long_dock_flow, "codex", columns, rows, view="graph", unicode=not ascii_only, navigation=nav)
        assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines())
        band = next(b for b in nav["bands"] if b["name"] == "detail")
        assert choose(("scroll", 3, 5, band["y1"] + 1), nav) is None
        assert nav["band_offsets"]["detail"] > 0
        render(long_dock_flow, "codex", columns, rows, view="graph", unicode=not ascii_only, navigation=nav)
        assert all(1 <= h["y"] <= rows and 1 <= h["x1"] <= h["x2"] <= columns for h in nav["hits"])

# The tab badge counts unread mail to mains only; the header shows the time, and a main without a task title shows its
# latest message; failed calls whose result their main already read fold into the finished count.
counted = deepcopy(flow)
# A result kept from before the write-time rule counts only when it went to the call's own parent.
kept_room = {"participants": [{"participant": "kept-call", "role": "worker", "parent": "kept-main"}]}
assert graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff",
                                             "recipient": "kept-main"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff",
                                                 "recipient": "other-main"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "intruder", "message_kind": "handoff"})
assert not graph_text.result_handoff(kept_room, {"id": "result-kept-call", "sender": "kept-call", "message_kind": "handoff"})
# A broadcast stays pending for calls that ended but remain joined; only calls that can still read count.
counted["room"]["pairs"] = [{"sender": "flow-main", "recipient": "flow-other", "sent": 1, "pending": 1},
                            {"sender": "flow-main", "recipient": "done-call", "sent": 2, "pending": 2},
                            {"sender": "flow-main", "recipient": "explorer", "sent": 3, "pending": 3}]
counted["room"]["participants"][0]["label"] = "Native task not recorded"
counted["room"]["messages"] = [{"sender": "flow-other", "ts": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 180)),
                                "targets": ["flow-main"], "pending_for": ["flow-other"], "text": "Call exit=0; parent acceptance pending.\nRebasing the parser"},
                               {"id": "result-explorer", "sender": "explorer", "recipient": "flow-attempt", "message_kind": "handoff", "targets": ["flow-main"], "pending_for": [], "text": "Call exit=1;"}]
for attempt in counted["attempts"]["active_recent"]:
    if attempt["attempt_id"] == "explorer":
        attempt["state"] = "failed"
picture = render(counted, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"dismissed": True})
first_line = picture.splitlines()[0]  # row 1 is the header, row 2 the usage line
assert "unread for live participants" not in first_line and re.search(r"\d\d:\d\d$", first_line), first_line
# Calls in review or blocked have ended; broadcasts still pending for them are not unread anyone can act on.
ended_members = [{"participant": "um", "role": "main"}, {"participant": "ur", "role": "worker", "parent": "um", "state": "review"},
                 {"participant": "ub", "role": "worker", "parent": "um", "state": "blocked"},
                 {"participant": "uw", "role": "worker", "parent": "um", "state": "working"}]
ended_pairs = {"room": {"pairs": [{"recipient": who, "pending": n} for who, n in (("ur", 40), ("ub", 7), ("uw", 2), ("um", 1))]}}
ended_pairs["room"]["messages"] = [{"pending_for": ["ur", "ub"]}, {"pending_for": ["uw", "um"]},
                                     {"pending_for": ["uw"]}, {"pending_for": ["um"]}]
assert graph_text.live_unread(ended_pairs, ended_members, lambda m: False) == 3
assert "Task: none claimed" in picture and "Latest sent" not in picture, picture
assert "explorer" not in picture and "Past work" not in picture and "Past 1" in picture, picture
pic_rows = picture.splitlines()
goal_at = next(i for i, line in enumerate(pic_rows) if line.startswith("╭─ Goal"))
main_row = next(i for i, line in enumerate(pic_rows) if "MAIN " in line)
assert goal_at == 2 and main_row > goal_at and "No goal recorded" in pic_rows[goal_at + 1], picture
assert "LANDING" not in picture, picture
no_plan_nav = {"dismissed": True}
render(counted, "codex", 157, 34, view="graph", main_attempt="flow-attempt", navigation=no_plan_nav)
assert {"y": 3, "x1": 1, "x2": 157, "action": ("tab", "plan")} in no_plan_nav["hits"], no_plan_nav["hits"][:4]
ascii_none = render(counted, "codex", 100, 34, view="graph", main_attempt="flow-attempt", unicode=False,
                    navigation={"dismissed": True}).splitlines()[2]
assert ascii_none.startswith("+- Goal") and ascii_none.isascii(), ascii_none
landing = deepcopy(counted)
landing["land"] = {"active": True, "sha": "abcdef0123", "step": "ci", "minutes": 7}
shown = render(landing, "codex", 157, 34, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
assert "LANDING abcdef0 ci" in shown.splitlines()[-2] and "LANDING" not in shown.splitlines()[2], shown
landing["land"]["active"] = False
assert "LANDING" not in render(landing, "codex", 157, 34, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
declared_board = deepcopy(counted)
declared_board["room"]["statuses"] = {"flow-main": {"text": "Fixing the parser", "ts": "2026-10-07T09:05:00Z", "seq": 3}}
declared_board["main_activity"] = {"flow-main": {"state": "idle", "attempt": "flow-attempt", "since": time.time() - 60}}
declared_picture = render(declared_board, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                 navigation={"dismissed": True})
assert "Now: Fixing the parser" in declared_picture and "(stale)" not in declared_picture and "Latest sent:" not in declared_picture, declared_picture
assert "Task: none claimed" in declared_picture, declared_picture
working_board = deepcopy(declared_board)
working_board["main_activity"]["flow-main"] = {"state": "busy", "attempt": "flow-attempt", "since": time.time() - 60}
working_picture = render(working_board, "codex", 157, 34, view="graph", main_attempt="flow-attempt",
                         navigation={"dismissed": True})
assert "Now: Fixing the parser" in working_picture and "(stale)" in working_picture, working_picture
long_status = deepcopy(declared_board["room"])
long_status["statuses"]["flow-main"]["text"] = "Checking the parser and cached dashboard projection for a very long declared status"
status_rows = graph_text.declared_status(long_status, "flow-main", 28, time.time())
assert len(status_rows) == 2 and "(stale)" in status_rows[-1] and all(display_width(row) <= 28 for row in status_rows), status_rows
declared_board["plan"] = {"tasks": [
    {"id": "old", "title": "Old claim", "state": "claimed", "claimed_by": "flow-main", "claimed_at": "2026-10-10T09:00:00Z"},
    {"id": "current", "title": "Current claim", "state": "running", "claimed_by": "flow-main", "claimed_at": "2026-10-10T10:00:00Z"},
    {"id": "cancelled", "state": "cancelled", "claimed_by": "flow-main", "claimed_at": "2026-10-10T11:00:00Z"}]}
held = graph_text.claims_by_main(declared_board)
assert [t["id"] for t in held["flow-main"]] == ["current", "old"], held
claimed_card = graph_text.render_graph(declared_board, 127, 50, navigation={"dismissed": True})
assert "Task: current · running · +1 more running" in claimed_card and "+2 more" not in claimed_card, claimed_card
claim_detail = graph_text.detail(dict(main, title="Native task not recorded"), {}, 100, people, {}, claims=held)[0]
assert "Task: Current claim" in claim_detail, claim_detail
declared_board["plan"] = deepcopy(counted.get("plan", {}))
working_picture = render(working_board, "codex", 110, 40, view="graph", main_attempt="flow-attempt",
                 navigation={"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}})
assert re.search(r"Now: Fixing the parser \(\d\d:\d\d\) \(stale\)", working_picture), working_picture
# A board too small for the box keeps the one-row tab strip: its names switch tabs, and no hint names a closed box.
for glyphs in (True, False):
    strips = 0
    for rows_high, small_report in ((rows_high, report) for rows_high in range(16, 33) for report in (counted, with_calls(counted, 3))):
        small_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
        small = render(small_report, "codex", 80, rows_high, view="graph", main_attempt="flow-attempt", navigation=small_nav, unicode=glyphs)
        tab_hits = [h for h in small_nav["hits"] if h["action"][0] == "tab"]
        tab_hits = [h for h in tab_hits if h["y"] == max(t["y"] for t in tab_hits)]
        if len(tab_hits) < 4:
            continue
        small_rows = small.split("\n")
        strip_y = tab_hits[0]["y"]
        assert len(small_rows) == rows_high and {h["y"] for h in tab_hits} == {strip_y}, (rows_high, len(small_rows), tab_hits, small)
        assert [h["action"][1] for h in tab_hits] == [key for key, _ in graph_text.TABS if key != "debate"], (rows_high, tab_hits)
        for h in tab_hits:
            assert small_rows[strip_y - 1][h["x1"] - 1:h["x2"]].strip("[] ").startswith(
                {"log": "Log", "detail": "Detail", "plan": "Plan", "debate": "Debate", "messages": "Messages", "past": "Past", "needs": "Attention"}[h["action"][1]]), small
        if not small_rows[strip_y].startswith(("│", "|")):
            strips += 1
            assert "Esc Close" not in small_rows[-1] and "f Full" not in small_rows[-1] and (glyphs or not any(c in small for c in "╭╮╰╯│─")), small
    assert strips, "no height showed the bare tab strip"
# The same bare strip includes Debate when this room has a recorded council.
with_debate = deepcopy(counted)
with_debate["attempts"].setdefault("recent", []).append({"attempt_id": "strip-council", "tool": "panel-council",
    "task_id": "strip-debate", "state": "done", "parent_attempt_id": "flow-attempt",
    "panel": {"room_id": "flow-room", "room_participant": "flow-main", "role": "advisor"}})
with_debate_nav = {}
render(with_debate, "codex", 80, 16, view="graph", navigation=with_debate_nav)
assert "debate" in with_debate_nav["visible_tabs"] and any(h["action"] == ("tab", "debate") for h in with_debate_nav["hits"])
# An answered advisor or reviewer stays above its main for half an hour, then moves to the Past tab.
import time as linger_clock
for age, kept in ((60, True), (3600, False)):
    answered = deepcopy(flow)
    answered["room"]["participants"] = [p for p in answered["room"]["participants"] if p.get("role") == "main"
                                        or p["participant"] == "reviewer"]
    for attempt in answered["attempts"]["active_recent"]:
        if attempt["attempt_id"] == "reviewer":
            attempt["state"] = "done"
    answered["room"]["call_results"] = {"reviewer": {"exit": 0, "read": True, "seq": 9, "ts": linger_clock.strftime(
        "%Y-%m-%dT%H:%M:%SZ", linger_clock.gmtime(linger_clock.time() - age))}}
    answered_lanes = render(answered, "codex", 160, 50, view="graph", navigation={"dismissed": True})
    assert ("REVIEWER Fable" in answered_lanes and "Past 1" not in answered_lanes) is kept, answered_lanes
    assert ("Past 1" in answered_lanes) is not kept and "Past work" not in answered_lanes, answered_lanes
    # Attention-only views still hide settled calls, lingering or not.
    assert "REVIEWER Fable" not in render(answered, "codex", 160, 50, view="graph", attention_only=True,
                                    navigation={"dismissed": True}), answered_lanes
# Reviewers above MAIN mirror the call tree: the trunk rises out of MAIN, so the top row (or the fold) takes the corner.
for glyphs, (top, mid) in ((True, ("┌╌", "├╌")), (False, (",-", "+-"))):
    for reviewers, rows_high in ((3, 50), (8, 24)):
        upper = crowd(flow, 0, 0, parent="flow-main", prefix="up")
        for n in range(reviewers):
            ident = "up-reviewer-%d" % n
            upper["room"]["participants"].append({"participant": ident, "role": "reviewer", "joined": True,
                                                  "model": "gpt-6-sol", "parent": "flow-main", "label": ident})
            upper["attempts"]["active_recent"].append({"attempt_id": ident, "state": "working", "panel": {
                "room_id": "flow-room", "room_participant": ident, "role": "reviewer"}})
        upper_nav = {"dismissed": True}
        upper_lines = render(upper, "codex", 160, rows_high, view="graph", main_attempt="flow-attempt",
                             navigation=upper_nav, unicode=glyphs).splitlines()
        main_y = next(i for i, row in enumerate(upper_lines) if "MAIN" in row)
        rows = [(hit["y"], upper_lines[hit["y"] - 1]) for hit in upper_nav["hits"] if hit.get("tree") and hit["y"] <= main_y]
        assert rows and all(y < main_y + 1 for y, _ in rows), (glyphs, reviewers, upper_lines)
        assert rows[0][1].lstrip().startswith(top) and all(row.lstrip().startswith(mid) for _, row in rows[1:]), (glyphs, rows)
        assert not any("└╌" in row or "`-" in row for _, row in rows), (glyphs, rows)
        fold = [row for _, row in rows if " more " in row]
        assert len(fold) <= 1 and (not fold or fold[0] is rows[0][1]), (glyphs, reviewers, rows)
lonely = deepcopy(flow)
lonely["room"]["participants"] = [p for p in lonely["room"]["participants"] if p["participant"] == "flow-main"]
alone = render(lonely, "codex", 120, 30, view="graph", main_attempt="flow-attempt", navigation={"dismissed": True})
assert "Workers 0/0" in alone and "Judges 0/0" in alone, alone
assert json.dumps(flow, sort_keys=True) == flow_before
wide_flow = deepcopy(flow)
wide_flow["room"]["messages"] = [
    {"sender": "advisor", "targets": ["flow-main"], "pending_for": ["flow-main"], "text": "CHECK FIXTURE FIRST"},
    {"sender": "advisor", "targets": ["flow-other"], "pending_for": ["flow-other"], "text": "PRIVATE OTHER MAIL"}]
wide_flow["room"]["pairs"] = [{"recipient": "flow-main", "pending": 1}]
wide_flow["room"]["participants"][0]["label"] = "Native task not recorded"
for columns in (139, 140, 141, 200):
    nav = {}
    drawn = render(wide_flow, "codex", columns, 44, view="graph", main_attempt="flow-attempt", navigation=nav)
    assert all(display_width(line) <= columns for line in drawn.splitlines()) and len(drawn.splitlines()) <= 44
    # The selected main's mail is read in the bottom Detail tab; another main's mail never shows.
    mail_drawn = render(wide_flow, "codex", columns, 44, view="graph", main_attempt="flow-attempt",
                        navigation={"band_offsets": {"detail": 99}})
    assert "CHECK FIXTURE FIRST" not in mail_drawn and "PRIVATE OTHER MAIL" not in mail_drawn and "Mail 2" in mail_drawn, mail_drawn
    assert "Task: none claimed" in drawn and "Native task not recorded" not in drawn
    for hit in nav["hits"]:
        # Card hits stay inside the card; the full-width detail area is a separate target.
        if hit["action"] == ("chat", "flow-main") and hit["y"] > 6 and not hit.get("preview"):
            assert hit["x1"] > 1 and hit["x2"] < columns, hit
expanded_main = deepcopy(wide_flow)
expanded_main["room"]["participants"][0]["label"] = "Fix caching and the panel integration while keeping every recorded result reachable"
for glyphs in (False, True):
    expanded_nav = {"dismissed": True}
    expanded_picture = render(expanded_main, "codex", 127, 40, view="graph", unicode=glyphs, navigation=expanded_nav)
    main_border = next(line for line in expanded_picture.splitlines() if "MAIN " in line)
    assert len(main_border) - len(main_border.lstrip()) == 6, expanded_picture
    assert "caching and the panel integration" in expanded_picture, expanded_picture
from panel_view import PALETTE
colored = render(wide_flow, "codex", 200, 44, view="graph", color=True)
# Main colour is stable across providers; role spans retain their own palette.
assert PALETTE["codex"] == PALETTE["main"] and PALETTE["worker"] != PALETTE["main"]
assert PALETTE["worker"] in colored and PALETTE["main"] in colored, colored
# Light terminal themes get darker shades of the same hues.
for theme_env, shade in (({"OMS_PANEL_THEME": "light"}, "130"), ({"COLORFGBG": "0;15"}, "130"),
                         ({"COLORFGBG": "15;0"}, "215"), ({"OMS_PANEL_THEME": "dark", "COLORFGBG": "0;15"}, "215")):
    picked = subprocess.run([sys.executable, "-c", "import panel_view; print(repr(panel_view.PALETTE['main']))"],
                            env=dict(os.environ, TERM="xterm-256color", PYTHONPATH=str(Path(panel.__file__).parent), **theme_env),
                            capture_output=True, text=True, check=True).stdout
    assert "38;5;" + shade + "m" in picked, (theme_env, picked)
flow_publication = deepcopy(flow)
flow_publication["room"]["publications"] = {"claude": {"status": "submitted", "delivery_unknown": True}}
for columns, rows in ((76, 16), (100, 22), (100, 48)):
    for menu in (False, True):
        nav = {}
        picture = render(flow_publication, "codex", columns, rows, view="graph", menu=menu, navigation=nav)
        assert "APP DELIVERY" not in picture and "att_" not in picture, picture
        from panel_view import inbox_items
        assert not any(item["who"] == "Main wake-up delivery failed" for item in inbox_items(flow_publication))
        failed_publication = deepcopy(flow_publication)
        failed_publication["room"]["publications"]["claude"].update(status="timeout", attempt="att_private")
        failed_publication["room"]["publications"]["codex"] = {"status": "rpc_error", "attempt": "att_other"}
        notices = [item for item in inbox_items(failed_publication) if item["who"] == "Main wake-up delivery failed"]
        assert len(notices) == 1 and "(unconfirmed)" in notices[0]["what"] and "att_" not in str(notices), notices
        assert len(picture.splitlines()) <= rows - int(menu) and all(display_width(line) <= columns for line in picture.splitlines())
navigation = {}
connected = render(tree_room, "codex", 91, 99, view="tree", navigation=navigation)
usage_row = connected.splitlines()[1]
assert "Claude" in usage_row and "Codex" in usage_row and connected.count("Codex --") + connected.count("Codex week") == 1, connected
assert "Third main" not in connected and "연결 근거 없는 작업" in connected and "Calls with no known main" in connected, connected
assert "No joined worker" not in connected and "participants outside graph" not in connected
# One root: a main with live attempt evidence, never the joined main that has none.
tree_roots = [i for i in navigation["items"] if i[0] == "chat"]
assert len(tree_roots) == 1 and tree_roots != [("chat", "extra-main")], navigation
target = next(hit for hit in navigation["hits"] if hit["action"][0] == "result")
assert choose(("click", 8, target["y"]), navigation) == target["action"]
assert choose(("click", 92, target["y"]), navigation) is None
assert choose(("click", 1, 100), navigation) is None
navigation["selected"] = ("result", "gone")
assert choose(("enter",), navigation) is None
owner_hit = next(hit for hit in navigation["hits"] if hit.get("fold"))
assert choose(("click", 1, owner_hit["y"]), navigation) == ("fold", owner_hit["action"][1])
navigation["collapsed"] = {owner_hit["action"][1]}
folded = render(tree_room, "codex", 91, 99, view="tree", navigation=navigation)
assert "calls" in folded and owner_hit["action"] in navigation["items"]

def visible_tree_rows(picture):
    return [re.sub(r"\033\[[0-9;]*m", "", line) for line in picture.splitlines()]

def tree_character_at(line, column):
    position = 0
    for character in line:
        width = display_width(character)
        if position == column:
            return character
        if position < column < position + width:
            return ""
        position += width
    return ""

for unicode_tree in (True, False):
    top_connector = "┌╌╌ " if unicode_tree else ",-  "
    child_connectors = ("├╌╌ ", "└╌╌ ") if unicode_tree else ("+-  ", "`-  ")
    vertical = "│" if unicode_tree else "|"
    layout_nav = {"expanded_groups": {("review", layout_main)}}
    layout_picture = render(layout_tree, "codex", 100, 40, view="tree", unicode=unicode_tree,
                            navigation=layout_nav)
    layout_rows = visible_tree_rows(layout_picture)
    first_heading = next(line for line in layout_rows if "REVIEWERS (2)" in line)
    activity_heading = next(line for line in layout_rows if "Activity" in line and "calls" in line)
    origin = display_width(activity_heading[:activity_heading.index("Activity")]) - 1
    assert display_width(first_heading[:first_heading.index(top_connector)]) == origin + 2, first_heading
    main_row = next(index for index, line in enumerate(layout_rows) if "Layout main" in line and "MAIN" in line)
    heading_row = layout_rows.index(first_heading)
    assert heading_row < main_row and all(tree_character_at(line, origin + 2) == vertical
                                          for line in layout_rows[heading_row + 1:main_row]), layout_picture
    for ident in ("layout-review-a", "layout-review-b", "layout-builder", "layout-awaiting"):
        hit = next(hit for hit in layout_nav["hits"] if hit["action"] == ("result", ident))
        row = layout_rows[hit["y"] - 1]
        branch_at = min(row.index(connector) for connector in child_connectors if connector in row)
        assert display_width(row[:branch_at]) - origin == 6, (ident, row, origin)

    selected_action = ("group", ("review", layout_main))
    for colored in (False, True):
        selected_nav = {"selected": selected_action}
        selected_picture = render(layout_tree, "codex", 100, 40, view="tree", unicode=unicode_tree,
                                 color=colored, navigation=selected_nav)
        selected_rows = visible_tree_rows(selected_picture)
        selected_hit = next(hit for hit in selected_nav["hits"] if hit["action"] == selected_action)
        row = selected_rows[selected_hit["y"] - 1]
        endpoint = "└╌╌ " if unicode_tree else "`-  "
        selected_glyph = "▸" if unicode_tree else ">"
        assert tree_character_at(row, origin) == selected_glyph and tree_character_at(row, origin + 1) == " ", row
        assert display_width(row[:row.index(endpoint)]) == origin + 2, row

    for columns in (100, 63):
        for colored in (False, True):
            expanded_nav = {"selected": ("result", "layout-review-a")}
            expanded_picture = render(layout_tree, "codex", columns, 40, view="tree", unicode=unicode_tree,
                                      color=colored, navigation=expanded_nav)
            expanded_rows = visible_tree_rows(expanded_picture)
            expanded_heading = next(index for index, line in enumerate(expanded_rows) if "REVIEWERS (2)" in line)
            expanded_main = next(index for index, line in enumerate(expanded_rows)
                                 if "Layout main" in line and "MAIN" in line)
            activity_heading = next(line for line in expanded_rows if "Activity" in line and "calls" in line)
            expanded_origin = display_width(activity_heading[:activity_heading.index("Activity")]) - 1
            broken_trunk = [line for line in expanded_rows[expanded_heading + 1:expanded_main]
                            if tree_character_at(line, expanded_origin + 2) != vertical]
            assert not broken_trunk, (unicode_tree, columns, colored, expanded_origin, broken_trunk,
                                      expanded_picture)
    # Below 40 columns the selected card has no left padding; the marker must not eat its corner.
    narrow_rows = visible_tree_rows(render(layout_tree, "codex", 39, 40, view="tree", unicode=unicode_tree,
                                           navigation={"selected": ("result", "layout-review-a")}))
    card_tops = [line for line in narrow_rows if "REVIEWER Luna" in line and "running" not in line]
    assert card_tops and ("╭" if unicode_tree else "+") in card_tops[0][:card_tops[0].index("REVIEWER")], narrow_rows

for columns, rows in ((91, 99), (42, 24), (28, 16), (28, 12), (2, 8), (1, 1)):
    for ascii_only in (False, True):
        for menu in (False, True):
            nav = {}
            drawn = render(tree_room, "codex", columns, rows, view="tree", unicode=not ascii_only,
                           menu=menu, navigation=nav)
            assert len(drawn.splitlines()) <= rows - int(menu), drawn
            assert all(display_width(line) <= columns for line in drawn.splitlines()), drawn
            assert all(1 <= hit["y"] <= len(drawn.splitlines()) for hit in nav["hits"])
            if ascii_only:
                assert "─" not in drawn and "│" not in drawn and "▾" not in drawn
            choose(("end",), nav)
            resized = render(tree_room, "codex", columns, rows, view="tree", navigation=nav)
            if nav["viewport"] and nav["selected"]:
                assert any(h["action"] == nav["selected"] for h in nav["hits"]), (columns, rows, menu, nav, resized)
# Keyboard order keeps the full tree; selection scrolls beyond the first viewport.
full_tree_nav = {}
render(flow, "codex", 42, 90, view="tree", navigation=full_tree_nav)
full_tree_order = full_tree_nav["items"]
traversal = {}
render(flow, "codex", 42, 24, view="tree", navigation=traversal)
reached = set()
for unused in range(30):
    choose(("down",), traversal)
    traversed = render(flow, "codex", 42, 24, view="tree", navigation=traversal)
    assert traversal["items"] == full_tree_order, traversal
    reached.add(traversal["selected"])
    assert any(hit["action"] == traversal["selected"] for hit in traversal["hits"]), (traversal, traversed)
assert reached == set(full_tree_order), (reached, full_tree_order)
for key in ("home", "end"):
    choose((key,), traversal)
    traversed = render(flow, "codex", 42, 24, view="tree", navigation=traversal)
    assert any(hit["action"] == traversal["selected"] for hit in traversal["hits"]), (key, traversal, traversed)
wheel_tree = {}
render(flow, "codex", 42, 24, view="tree", navigation=wheel_tree)
choose(("home",), wheel_tree)
render(flow, "codex", 42, 24, view="tree", navigation=wheel_tree)
choose(("scroll", 3), wheel_tree)
wheel_offset = wheel_tree["offset"]
assert wheel_offset > 0, wheel_tree
render(flow, "codex", 42, 24, view="tree", navigation=wheel_tree)
assert wheel_tree["offset"] == wheel_offset, wheel_tree
assert json.dumps(tree_room, sort_keys=True) == before_tree
# ASCII mode never emits a byte above 0x7F in the tree, the list or the menu (data labels aside).
plain_room = deepcopy(tree_room)
for member in plain_room["room"]["participants"]:
    member["label"] = "plain label"
for row in plain_room["attempts"]["active_recent"] + plain_room["attempts"]["recent"]:
    row.get("panel", {})["label"] = "plain label"
for columns, rows in ((120, 40), (100, 30), (80, 24), (54, 24), (42, 16)):
    for kind in ("tree", "list", "summary", "graph"):
        for menu in (False, True):
            ascii_nav = {}
            drawn = render(plain_room, "codex", columns, rows, view=kind, unicode=False, menu=menu, navigation=ascii_nav)
            assert all(ord(ch) < 128 for ch in drawn), (columns, rows, kind, menu, [ch for ch in drawn if ord(ch) > 127], drawn)
            if kind == "tree" and not menu:
                assert ascii_nav["surface"] == "tree", ascii_nav
            elif kind == "graph":
                assert ascii_nav["surface"] == ("graph" if columns >= 76 else "tree"), ascii_nav
# The tree speaks in words: usage card, unread scope, an old plan, and calls without lifecycle evidence.
words = deepcopy(tree_room)
words["acceptance"] = {"available": True, "total": 6, "counts": {}, "complete": False}
words["plan"] = {"idle_days": 44}
words["operations"] = {"recent": [{"kind": "call", "event_id": "w1", "selected_model": "gpt-6-sol", "tokens": 1100000}]}
speech = render(words, "codex", 120, 60, view="tree", navigation={})
assert "Old plan · idle 44 days · 0 of 6 tasks verified" in speech and "Model use, last 8 calls" not in speech, speech
assert "USAGE" not in speech and "Sol 6 1.1m tokens ×1" not in speech, speech
assert "unread for live participants" not in speech.splitlines()[0], speech
for stale in ("PROVIDER LIMITS", "(sel)", " tok / ", "Plan:"):
    assert stale not in speech, (stale, speech)
assert "status unknown" in speech and "no lifecycle record" not in speech, speech
# Calls whose attempts left the projection take their end state from the result messages: finished above, running below.
def minutes_ago(minutes):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - minutes * 60))
ended = deepcopy(flow)
ended["room"]["call_results"] = {}
for n in range(14):
    ident = "old-%02d" % n
    ended["room"]["participants"].append({"participant": ident, "role": "worker", "model": "gpt-6-luna", "joined": True,
        "provider": "codex", "parent": "flow-attempt", "label": "Old job %02d" % n, "seq": 10 + n,
        "joined_at": minutes_ago(300 - n * 10)})
    ended["room"]["call_results"][ident] = {"exit": 1 if n == 0 else 0, "ts": minutes_ago(290 - n * 10), "seq": 100 + n}
ended["room"]["call_results"]["old-03"]["exit"] = None
ended_text = render(ended, "codex", 120, 120, view="tree", navigation={})
assert "status unknown" in ended_text and "failed: exit 1" in ended_text, ended_text
ended_body = ended_text.split("╭─ [Detail]", 1)[0]
# Finished calls leave the tree; the Past tab lists each one, newest first, with how and when it ended.
assert "Old job 03" in ended_body and "finished" not in ended_body and "Past work" not in ended_body, ended_text
ended_past = render(ended, "codex", 120, 120, view="tree", navigation={"tab": "past"}).split("[Past 12]", 1)[1]
past_jobs = [line for line in ended_past.splitlines() if "Old job" in line]
# The first page lists the newest calls in order (at least eleven at this height); the header counts every one.
newest_first = [n for n in range(13, 0, -1) if n != 3]
assert "12 past calls, newest first" in ended_past and len(past_jobs) >= 11 and \
    [int(line.split("Old job ")[1][:2]) for line in past_jobs] == newest_first[:len(past_jobs)], past_jobs
# An end time from an earlier local day carries its date, so the expectation holds across midnight.
def ended_stamp(n):
    import calendar
    local = time.localtime(calendar.timegm(time.strptime(ended["room"]["call_results"]["old-%02d" % n]["ts"], "%Y-%m-%dT%H:%M:%SZ")))
    return time.strftime("%H:%M" if local[:3] == time.localtime()[:3] else "%m-%d %H:%M", local)
assert all(re.search(r"WORKER Luna 6\s+· Old job %02d\s+\[✓ finished: exit 0\] \[acceptance pending\]  %s " % (n, ended_stamp(n)), line + " ")
           for line, n in zip(past_jobs, newest_first)), past_jobs
# A worker that hit its wall clock says how to continue; later rounds of one task are numbered.
timed = deepcopy(ended)
timed["room"]["call_results"]["old-00"]["exit"] = 124
timed_text = render(timed, "codex", 120, 120, view="tree", navigation={})
assert "timed out" in timed_text and "--continue" not in timed_text, timed_text
timed_worker = next(m for m in graph_text.nodes(timed) if m["participant"] == "old-00")
timed_detail = "\n".join(graph_text.detail(timed_worker, {}, 100, graph_text.nodes(timed))[0])
assert "timed out" in timed_detail.splitlines()[0] and "failed" not in timed_detail.splitlines()[0], timed_detail
assert "timed out" in timed_detail and "Timed out · its main can continue it" in timed_detail and "oms panel" not in timed_detail
from room_view import nodes as room_nodes
rounded = deepcopy(ended)
rounded["attempts"] = {"active_recent": [], "recent": [
    {"attempt_id": "a-%s" % ident, "state": "failed", "task_id": "same-task",
     "panel": {"role": "worker", "room_participant": ident, "room_id": rounded["room"].get("id")}}
    for ident in ("old-00", "old-01")]}
numbered = {m["participant"]: m for m in room_nodes(rounded)}
assert numbered["old-00"]["round"] == 1 and numbered["old-01"]["round"] == 2 and numbered["old-01"]["continues"] == "old-00", numbered
rows = [line for line in ended_body.splitlines() if ("Luna" in line or "Sonnet" in line) and ("failed" in line or "running" in line or "no record" in line)]
order = [2 if "running" in r else 1 for r in rows]
assert 1 in order and 2 in order, rows
# A call outside the projection takes its real state from room_attempts, ahead of any exit code.
gone = deepcopy(ended)
for ident in ("gone-review", "gone-blocked", "gone-live"):
    gone["room"]["participants"].append({"participant": ident, "role": "worker", "model": "gpt-6-luna", "joined": True,
        "provider": "codex", "parent": "flow-attempt", "label": "Gone " + ident, "seq": 40})
gone["room"]["call_results"]["gone-review"] = {"exit": 0, "ts": minutes_ago(5), "seq": 200}
gone["room_attempts"] = {"gone-review": {"state": "review", "attempt_id": "att-r", "updated_at": minutes_ago(4), "task_id": "task-r"},
                         "gone-blocked": {"state": "blocked", "attempt_id": "att-b", "updated_at": minutes_ago(4), "task_id": "task-b"},
                         "gone-live": {"state": "working", "attempt_id": "att-live", "updated_at": minutes_ago(4),
                                       "task_id": "task-live", "lease_id": "lease-live"}}
seen = {m["participant"]: m for m in graph_text.nodes(gone)}
assert (seen["gone-review"]["state"], seen["gone-review"]["attempt"], seen["gone-review"]["task_id"]) == ("review", "att-r", "task-r"), seen["gone-review"]
assert seen["gone-blocked"]["state"] == "blocked" and seen["old-01"]["state"] == "done", seen
assert seen["gone-live"]["state"] == "working" and seen["gone-live"]["lease_id"] == "lease-live", seen["gone-live"]
gone["plan"] = {"tasks": [{"id": "task-live", "state": "running", "claimed_by": "flow-attempt",
                            "lease_id": "lease-live", "claimed_at": minutes_ago(1)}]}
assert graph_text.held_split(gone) == (set(), set()), graph_text.held_split(gone)
# snapshot() links full-projection rows only on the room, participant and role identity, newest row winning.
from unittest.mock import patch
import room as room_module
def event(attempt, state, at, room_id="flow-room", who="gone-review", role="worker"):
    # The real ledger row shape: panel identity lives in refs, not in a "panel" mapping.
    return {"attempt_id": attempt, "state": state, "updated_at": at, "task_id": "t-" + attempt,
            "refs": {"panel_room_id": room_id, "panel_room_participant": who, "panel_role": role}}
listed = [event("a-old", "working", "2026-01-01T00:00:00Z"), event("a-new", "review", "2026-01-02T00:00:00Z"),
          event("a-room", "failed", "2026-01-03T00:00:00Z", room_id="other-room"),
          event("a-who", "failed", "2026-01-03T00:00:00Z", who="someone-else"),
          event("a-role", "failed", "2026-01-03T00:00:00Z", role="advisor")]
live_row = event("a-live", "working", "2026-01-03T00:00:00Z", who="gone-live")
live_row["refs"]["panel_lease_id"] = "lease-live"
listed.append(live_row)
members = [{"participant": "gone-review", "role": "worker", "joined": True},
           {"participant": "gone-live", "role": "worker", "joined": True, "parent": "flow-attempt"},
           {"participant": "flow-main", "role": "main", "joined": True}]
def fake_run(command, **kwargs):
    if command[2:3] == ["land"]:
        return subprocess.CompletedProcess(command, 0, json.dumps({"active": True, "sha": "abc1234", "step": "gate", "minutes": 2}), "")
    assert command[2:3] == [str(panel.ENTRY)] or "agent-events" in command, command
    return subprocess.CompletedProcess(command, 0, json.dumps(listed), "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "results", return_value={"rows": []}), \
        patch.object(panel, "outcomes", return_value={}), \
        patch.object(panel.subprocess, "run", side_effect=fake_run):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert got["land"] == {"active": True, "sha": "abc1234", "step": "gate", "minutes": 2}, got.get("land")
assert got["room_attempts"] == {"gone-review": {"state": "review", "attempt_id": "a-new",
    "updated_at": "2026-01-02T00:00:00Z", "task_id": "t-a-new", "lease_id": None},
    "gone-live": {"state": "working", "attempt_id": "a-live", "updated_at": "2026-01-03T00:00:00Z",
                  "task_id": "t-a-live", "lease_id": "lease-live"}}, got.get("room_attempts")
snapshot_nodes = {m["participant"]: m for m in graph_text.nodes(got)}
assert snapshot_nodes["gone-live"]["lease_id"] == "lease-live", snapshot_nodes["gone-live"]
got["plan"] = {"tasks": [{"id": "t-a-live", "state": "running", "claimed_by": "flow-attempt",
                            "lease_id": "lease-live", "claimed_at": "2026-01-03T00:00:01Z"}]}
assert graph_text.held_split(got) == (set(), set()), graph_text.held_split(got)
# An older review call found only in room_attempts still gets the owner's recorded decision.
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "outcomes", side_effect=lambda repo, ids, _shared=False, _tick=None: {"t-a-new": "accepted"} if "t-a-new" in ids else {}), \
        patch.object(panel.subprocess, "run", side_effect=fake_run):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert got.get("finalized") == {"t-a-new": "accepted"}, got.get("finalized")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "flow-room", "participants": members}), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired("agent-events", 5)):
    got, _ = panel.snapshot(Path("."), "flow-room")
assert "room_attempts" not in got and got["room"]["participants"] == members, got
# Admission belongs to the shared snapshot, including watch reads, not only the control menu.
admission_room = {"id": "admission-room", "participants": [
    {"participant": "admission-main", "role": "main", "joined": True},
    {"participant": "admission-worker", "role": "worker", "parent": "admission-main", "joined": True}]}
admission_state = {"repo": {"name": "x"}, "attempts": {"recent": [{
    "attempt_id": "admission-worker", "task_id": "admission-task", "state": "done",
    "panel": {"room_id": "admission-room", "room_participant": "admission-worker", "role": "worker", "access": "write"}}]}}
for pending_tasks in ({"admission-task"}, set()):
    with patch.object(panel, "dashboard", side_effect=lambda *a, **k: (deepcopy(admission_state), 0)), \
            patch.object(room_module, "status", side_effect=lambda *a: deepcopy(admission_room)), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel_results, "pending_admissions", return_value=pending_tasks) as pending, \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "[]", "")), \
            patch("panel_metrics.collect", return_value={}), \
            patch.dict(os.environ, {"OMS_ROOM_ID": "admission-room"}):
        watched = panel.read_panel(Path("."), "codex", "graph", False)[0]
        collected, _ = panel.snapshot(Path("."), "admission-room")
    expected = ["admission-worker"] if pending_tasks else []
    assert pending.call_count == 2 and all(call.args[1] == {"admission-task"} for call in pending.call_args_list)
    for report in (watched, collected):
        assert report.get("awaiting_admission") == expected, report
        worker = next(m for m in graph_text.nodes(report) if m["participant"] == "admission-worker")
        assert graph_text.call_classifier(report)(worker) == ("review" if expected else "past")
# Record-only admission reads every retained call, without loading patch or answer bytes.
admission_records = [
    {"kind": "delegate", "task_id": ident, "patch": ".oms/artifacts/change.patch", "patch_sha256": "a" * 64}
    for ident in ("pending", "admitted", "landed", "many-calls")]
admission_records += [{"kind": "patch-admit", "task_id": "admitted"},
                      {"kind": "patch-land", "task_id": "landed"}]
admission_records += [{"kind": "call", "task_id": "many-calls"} for _ in range(35)]
admission_records += [{"kind": "call", "task_id": "no-patch"}]
with patch.object(panel_results, "_records", return_value=([], admission_records)), \
        patch.object(panel_results, "_read", side_effect=AssertionError("admission read artifact bytes")), \
        patch("panel_cache.read_shared", side_effect=lambda repo, key, collect: collect()):
    assert panel_results.pending_admissions(Path("."), {"pending", "admitted", "landed", "many-calls", "no-patch", "missing"}) == {"pending", "many-calls"}
    assert panel_results.pending_admissions(Path("."), {"../bad"}) == set()
    # Only tasks without any retained rows are unknown at the coverage boundary.
    with patch.object(panel_results, "_records", return_value=([{}] * 300, admission_records)):
        assert panel_results.pending_admissions(Path("."), {"pending", "missing", "admitted", "landed", "no-patch"}) == {"pending", "missing"}
    with patch.object(panel_results, "_records", return_value=([], admission_records + [{}] * (1000 - len(admission_records)))):
        assert panel_results.pending_admissions(Path("."), {"pending", "missing", "admitted", "landed", "no-patch"}) == {"pending", "missing"}
# Gather the whole linked scope before applying the inbox cap; keep member order.
admission_members = [{"participant": "w-%s" % n, "role": "worker", "access": "write",
                      "state": "done", "task_id": "t-%s" % n} for n in range(panel.INBOX_LIMIT + 2)]
with patch("room_view.nodes", return_value=admission_members), \
        patch.object(panel_results, "pending_admissions", return_value={m["task_id"] for m in admission_members}) as pending:
    assert panel.awaiting_admission(Path("."), {}) == [m["participant"] for m in admission_members[:panel.INBOX_LIMIT]]
    assert pending.call_count == 1 and pending.call_args.args[1] == {m["task_id"] for m in admission_members}
# Both plan collectors retain tasks beyond their former 20/40 caps for pagination.
long_tasks = [{"id": "long-%02d" % i, "title": "Plan task %s" % i,
               "state": "ready" if i == 44 else "done"} for i in range(45)]
long_tasks[0]["lease_id"] = "lease_projection"
long_plan_repo = Path(sys.argv[2]) / "long-plan"
(long_plan_repo / ".oms/plan").mkdir(parents=True)
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": long_tasks}), encoding="utf-8")
from dashboard_projection import plan_tasks
assert len(plan_tasks(str(long_plan_repo))) == 45
projected = plan_tasks(str(long_plan_repo))
assert projected[0] == {"id": "long-00", "title": "Plan task 0", "state": "done", "lease_id": "lease_projection",
                       "claimed_by": None}, projected[0]
completion_tasks = [dict(row) for row in long_tasks]
completion_tasks[0]["completion_kind"] = "research-accepted"
completion_tasks[0]["claimed_at"] = "2026-10-10T10:00:00Z"
completion_tasks[1]["completion_kind"] = "satisfied-by"
completion_tasks[2]["completion_kind"] = {"kind": "research-accepted"}
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": completion_tasks}), encoding="utf-8")
projected = plan_tasks(str(long_plan_repo))
assert projected[0]["completion_kind"] == "research-accepted" and projected[1]["completion_kind"] == "satisfied-by", projected[:2]
assert "completion_kind" not in projected[2] and "completion_kind" not in projected[3], projected[2:4]
assert projected[0]["claimed_at"] == "2026-10-10T10:00:00Z" and "claimed_at" not in projected[1], projected[:2]
(long_plan_repo / ".oms/plan/tasks.json").write_text(json.dumps({"tasks": long_tasks}), encoding="utf-8")
def long_plan_run(command, **kwargs):
    data = {"tasks": long_tasks} if "agent-plan" in command else []
    return subprocess.CompletedProcess(command, 0, json.dumps(data), "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}, "plan": {"present": True}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "long-plan", "participants": []}), \
        patch.object(panel.subprocess, "run", side_effect=long_plan_run):
    long_report, _ = panel.snapshot(long_plan_repo, "long-plan")
assert long_report["room"]["repo_tasks"] == long_tasks
def slow_plan_run(command, **kwargs):
    if "agent-plan" in command:
        raise subprocess.TimeoutExpired(command, kwargs.get("timeout"))
    return subprocess.CompletedProcess(command, 0, "[]", "")
with patch.object(panel, "dashboard", return_value=({"repo": {"name": "x"}, "plan": {"present": True}}, 0)), \
        patch.object(room_module, "status", return_value={"id": "long-plan", "participants": [{"participant": "m1"}]}), \
        patch.object(panel.subprocess, "run", side_effect=slow_plan_run):
    slow_report, _ = panel.snapshot(long_plan_repo, "long-plan")
assert slow_report["room"].get("participants") and slow_report["room"].get("repo_tasks_unavailable"), slow_report["room"]
assert "error" not in slow_report["room"], "a slow plan list must not hide the room and its main"
with patch.object(panel, "dashboard", return_value=({"collection": {"ok": True}}, 0)), \
        patch.object(panel, "read_shared", side_effect=lambda repo, key, collect, **kwargs: collect()), \
        patch.object(room_module, "status", side_effect=ValueError("missing room evidence")), \
        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "{}", "")):
    unreadable_report, unreadable_status = panel.snapshot(long_plan_repo, "unreadable-room")
assert unreadable_status == 1 and unreadable_report["room"]["error"] == "room unreadable: ValueError: missing room evidence"

ended_nav = {"tab": "past"}
ended_picture = render(ended, "codex", 120, 120, view="tree", navigation=ended_nav)
activity_top = next(i + 1 for i, line in enumerate(ended_picture.splitlines()) if line.startswith("╭─ Activity"))
shown = list(dict.fromkeys(h["action"][1] for h in sorted(ended_nav["hits"], key=lambda h: h["y"]) if h["action"][0] == "result"))
assert shown == [i[1] for i in ended_nav["items"] if i[0] == "result"] and shown.index("old-13") > shown.index("builder"), shown
assert any(h["action"] == ("result", "old-00") and h["y"] > activity_top for h in ended_nav["hits"]), ended_nav
assert all(h.get("past") for h in ended_nav["hits"] if h["action"] == ("result", "old-13")), ended_nav
# With a second main the open Past list still gets a body on short trees, so its keys reach the list.
for columns, rows_high in ((42, 24), (80, 24), (110, 24), (110, 28)):
    two_nav = {"tab": "past"}
    two_past = render(ended, "codex", columns, rows_high, view="tree", navigation=two_nav, main_attempt="flow-attempt")
    assert two_nav["tab_body_visible"] and two_nav["box"] and "Old job 13" in two_past, (columns, rows_high, two_past)
    assert all(display_width(line) <= columns for line in two_past.splitlines()), two_past
# Past-list keys: arrows, Home and End pick a call and keep it on the page; a wheel scroll keeps the choice.
one_ended = deepcopy(ended)
one_ended["room"]["participants"] = [m for m in one_ended["room"]["participants"]
                                     if m["participant"] not in {"flow-other", "foreign-child", "unlinked-child"}]
keys_nav = {"tab": "past"}
render(one_ended, "codex", 57, 30, view="tree", navigation=keys_nav)
# Removing the usage box leaves extra list space; the Past reader still pages and preserves selection.
assert keys_nav["past_view"]["page"] >= 4, keys_nav["past_view"]
last_scroll = len(keys_nav["past_ids"]) - keys_nav["past_view"]["page"]
picked = []
for key in ("down", "down", "end", "up", "home"):
    assert panel.tab_event((key,), keys_nav), key
    render(one_ended, "codex", 57, 30, view="tree", navigation=keys_nav)
    picked.append((keys_nav["selected"][1], keys_nav["past_view"]["scroll"]))
end_scroll = picked[2][1]
assert picked == [("old-13", 0), ("old-12", 0), ("old-01", end_scroll), ("old-02", end_scroll), ("old-13", 0)], picked
assert end_scroll >= last_scroll, (end_scroll, last_scroll)
assert panel.tab_event(("scroll", 3, 5, keys_nav["box"]["y1"] + 2), keys_nav)
render(one_ended, "codex", 57, 30, view="tree", navigation=keys_nav)
assert keys_nav["selected"] == ("result", "old-13") and keys_nav["past_view"]["scroll"] == 3, keys_nav
assert panel.tab_event(("enter",), keys_nav) and keys_nav["tab"] == "detail" and \
    keys_nav["preview"] == {"target": ("result", "old-13")}, keys_nav
f_nav = {"tab": "past"}
render(one_ended, "codex", 57, 30, view="tree", navigation=f_nav)
assert panel.tab_event(("down",), f_nav) and panel.tab_event(("f",), f_nav)
assert f_nav["open_call"] == ("result", "old-13") and f_nav["tab"] == "detail", f_nav
# Narrow rows drop the role word and time before the task title, so rows still differ by task.
for columns in (28, 42):
    for glyphs in (True, False):
        narrow_past = render(one_ended, "codex", columns, 24, view="tree", unicode=glyphs, navigation={"tab": "past"})
        narrow_rows = [line for line in narrow_past.splitlines() if "finished" in line]
        assert narrow_rows and all(re.search(r"Old job \d{2}\b", line) for line in narrow_rows), narrow_rows
        assert len({re.search(r"Old job \d{2}\b", line)[0] for line in narrow_rows}) == len(narrow_rows), narrow_rows
# Wrapped Past entries are paged by physical height so End always reaches the last call.
wrapped_past_calls = [{"participant": "wrapped-%02d" % n, "role": "worker", "model": "gpt-6-luna",
                       "title": "Wrapped title %02d " % n + "x" * 100, "state": "done", "exit": 0,
                       "ended": "2026-10-20T00:%02d:00Z" % n, "seq": n} for n in range(6)]
wrapped_past_navigation = {"tab": "past"}
graph_text.past_body(wrapped_past_calls, 76, 5, wrapped_past_navigation, {}, True)
assert graph_text.past_event(("end",), wrapped_past_navigation)
wrapped_past_page = graph_text.past_body(wrapped_past_calls, 76, 5, wrapped_past_navigation, {}, True)
assert any(row[1] == ("result", "wrapped-05") for row in wrapped_past_page), wrapped_past_page
# Attention-only empties the whole Past list, owned and unowned calls alike.
orphan_done = deepcopy(ended)
next(a for a in orphan_done["attempts"]["active_recent"] if a["attempt_id"] == "unlinked-child")["state"] = "done"
for mode in ("tree", "graph"):
    for only in (False, True):
        only_nav = {"tab": "past"}
        render(orphan_done, "codex", 127, 60, view=mode, attention_only=only, navigation=only_nav)
        assert "past" in only_nav["visible_tabs"] and (not only or not only_nav.get("past_ids")), (mode, only, only_nav["visible_tabs"])
        assert only or len(only_nav["past_ids"]) == 13, only_nav["past_ids"]
# Empty Past remains visible and offers only tab navigation.
for mode in ("tree", "graph"):
    for columns, rows_high in ((57, 24), (127, 40)):
        hinted = [render(single_flow, "codex", columns, rows_high, view=mode, navigation={"tab": tab},
                         main_attempt="flow-attempt").splitlines()[-1] for tab in ("past", "detail")]
        assert "Tab Next tab" in hinted[0] and "Call" not in hinted[0] and "Full result" not in hinted[0], (mode, columns, hinted)
# A past debate opens in Detail from the tree's Past tab as it does from the graph.
past_debate = deepcopy(single_flow)
past_debate["attempts"]["active_recent"].append({"attempt_id": "council-1", "state": "done", "tool": "panel-council",
    "task_id": "deb-1", "parent_attempt_id": "flow-attempt", "panel": {"label": "Pick a design"}})
for mode in ("tree", "graph"):
    debate_nav = {"tab": "past"}
    render(past_debate, "codex", 127, 50, view=mode, navigation=debate_nav)
    assert debate_nav["past_ids"] == [("debate", ("deb-1", "flow-main"))], debate_nav
    debate_nav["surface"] = mode
    assert panel.tab_event(("down",), debate_nav) and panel.tab_event(("enter",), debate_nav)
    assert "COUNCIL " in render(past_debate, "codex", 127, 50, view=mode, navigation=debate_nav), mode
team_lines, _ =graph_text.detail(next(m for m in graph_text.nodes(ended) if m["participant"] == "flow-main"), {}, 120,
    graph_text.nodes(ended), ended["room"], everyone={"flow-main": [m for m in graph_text.nodes(ended)
                                                                  if m.get("parent") == "flow-attempt"]})
team_rows = [line for line in team_lines if "Old job" in line or "한글 작업 제목 / builder" in line]
assert "failed (exit 1)" in team_rows[0] and "finished" in team_rows[-1], team_rows
assert team_lines.index("  Needs attention") < team_lines.index("  Active") < team_lines.index("  Finished"), team_lines
assert sum("Old job" in line and "finished" in line for line in team_lines) == 5, team_lines
assert "7 earlier finished · see the Past tab" in team_lines, team_lines
empty_team = deepcopy(flow)
empty_team["room"]["participants"] = [m for m in flow_members if m.get("role") == "main"]
admission_team = deepcopy(flow)
next(a for a in admission_team["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = "done"
admission_team["awaiting_admission"] = ["builder"]
next(m for m in admission_team["room"]["participants"] if m["participant"] == "flow-main")["label"] = "Native task not recorded"
admission_team["plan"] = {"tasks": [{"id": "claim-detail", "title": "Current claim", "state": "running",
                                      "claimed_by": "flow-main", "claimed_at": "2026-10-11T12:00:00Z"}]}
# A failure is no longer attention once a later round of its task runs or its task is closed in the plan.
retried = deepcopy(ended)
retried["room"]["participants"].append({"participant": "retry-00", "role": "worker", "model": "gpt-6-luna", "joined": True,
    "provider": "codex", "parent": "flow-attempt", "label": "Old job 00 again", "seq": 60})
retried["attempts"]["active_recent"] += [{"attempt_id": ident, "state": state, "task_id": "retry-task", "panel": {
    "room_id": "flow-room", "room_participant": ident, "role": "worker"}} for ident, state in (("old-00", "failed"), ("retry-00", "working"))]
closed_failure = deepcopy(ended)
closed_failure["attempts"]["active_recent"].append({"attempt_id": "old-00", "state": "failed", "task_id": "retry-task",
    "panel": {"room_id": "flow-room", "room_participant": "old-00", "role": "worker"}})
closed_failure["plan"] = {"tasks": [{"id": "retry-task", "state": "cancelled"}]}
for settled_report in (retried, closed_failure):
    assert ("result", "old-00") not in {item["action"] for item in inbox_items(settled_report)}
    assert graph_text.call_classifier(settled_report)(next(m for m in graph_text.nodes(settled_report)
                                                           if m["participant"] == "old-00")) == "past"
assert ("result", "old-00") in {item["action"] for item in inbox_items(ended)}
for team_report, expected_team in ((flow, "5 active"),
                                   (ended, "1 attention · 5 active · 12 finished · 1 unrecorded"),
                                   (retried, "6 active · 13 finished · 1 unrecorded"),
                                   (closed_failure, "5 active · 13 finished · 1 unrecorded"),
                                   (admission_team, "4 active · 1 awaiting decision"),
                                   (empty_team, "no calls yet")):
    detail_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
    detail_team = graph_text.render_graph(team_report, 127, 120, navigation=detail_nav).splitlines()
    team_card = graph_text.render_graph(team_report, 127, 50, navigation={"dismissed": True})
    member = next(m for m in graph_text.nodes(team_report) if m["participant"] == "flow-main")
    full_team, full_targets = graph_text.detail(member, {}, 120, graph_text.nodes(team_report), team_report["room"],
        everyone={"flow-main": [m for m in graph_text.nodes(team_report) if m.get("parent") == "flow-attempt"]}, classify=graph_text.call_classifier(team_report))
    assert "Team: " + expected_team in full_team, (expected_team, full_team)
    assert "Team:" not in team_card and "WORK STATUS" not in team_card and "MAIL /" not in team_card, team_card
    if team_report is admission_team:
        assert ("result", "builder") in full_targets.values() and "claim-detail" in team_card, (full_team, team_card)
# A lifecycle review call and a finished call awaiting admission share the board's "Awaiting decision" count.
assert graph_text.team_counts([{"state": state} for state in
    ("presence unknown", "done", "working", "failed", "cancelled", "done", "review")],
    lambda call: "review" if call["state"] in {"done", "review"} else "live") == [
        "1 attention", "1 active", "3 awaiting decision", "1 cancelled", "1 unrecorded"]
assert graph_text.team_counts([{"state": "done"}, {"state": "cancelled"}]) == ["1 finished", "1 cancelled"]
assert any("status unknown" in line for line in team_lines) and "failed (exit 1)" in "\n".join(team_lines), team_lines
# Spare rows between the call tree and the box carry a display-only summary; the tree and the box keep every row.
summary_room = deepcopy(admission_team)
next(a for a in summary_room["attempts"]["active_recent"] if a["attempt_id"] == "flow-other")["state"] = "done"
summary_room["room"]["call_results"] = {"builder": {"exit": 0, "ts": minutes_ago(5), "seq": 9}}
# One running call leaves most of the tree's reserved rows blank above the box.
summary_room["room"]["participants"] = [m for m in summary_room["room"]["participants"]
                                        if m["participant"] not in {"advisor", "reviewer", "explorer"}]
next(a for a in summary_room["attempts"]["active_recent"] if a["attempt_id"] == "builder")["state"] = "done"
for columns, rows in ((127, 60), (127, 40), (110, 24), (80, 24), (42, 24)):
    for glyphs in (True, False):
        log_nav = {}
        log_screen = graph_text.render_graph(summary_room, columns, rows, unicode=glyphs, navigation=log_nav)
        assert len(log_screen.splitlines()) == rows and all(display_width(line) <= columns for line in log_screen.splitlines()), log_screen
        assert "In progress" not in log_screen and "Recent       " not in log_screen, log_screen
        assert not graph_text.session_log(dict(summary_room, room=dict(summary_room["room"], messages=[])), columns, 10, glyphs)
        assert not graph_text.session_log(summary_room, columns, 2, glyphs)
# Attempt evidence beats the result message, and the header clock carries date and weekday.
beaten = deepcopy(ended)
beaten["attempts"]["active_recent"].append({"attempt_id": "old-00", "state": "working",
    "panel": {"room_id": "flow-room", "room_participant": "old-00", "role": "worker"}})
assert next(m for m in graph_text.nodes(beaten) if m["participant"] == "old-00")["state"] == "working"
assert re.search(r"\d\d-\d\d (Mon|Tue|Wed|Thu|Fri|Sat|Sun) \d\d:\d\d$", graph_text.render_graph(flow, 120, 40).splitlines()[0])
board_text = graph_text.render_graph(words, 120, 40)
assert "Plan:" not in board_text, board_text
# An active plan puts its goal and progress below the header and usage on both single-main boards and the held task on the main's card.
shown_main = next(m["participant"] for m in graph_text.nodes(words) if m.get("role") == "main")
words["provider_status"] = {"claude": {"weekly_used": 12, "context_left": 80}, "codex": {"weekly_used": 30}}
words["plan"] = {"present": True, "goal": "Ship the shared plan board", "task_count": 9, "idle_days": 3,
                 "by_state": {"done": 6, "review": 1, "running": 2},
                 "tasks": [{"id": "t7", "title": "Wire", "state": "running", "claimed_by": shown_main}]}
banner_nav = {}
active = graph_text.render_graph(words, 120, 40, navigation=banner_nav)
active_rows = active.splitlines()
top_at = next(i for i, line in enumerate(active_rows) if line.startswith("╭─ Goal"))
assert top_at == 2 and active_rows[0].startswith("OMS · ") and "Main" in active_rows[1], active
assert "6/9 verified / 1 in review / 2 claimed" in active_rows[top_at] and display_width(active_rows[top_at]) == 120, active
assert "Ship the shared plan board" in active_rows[top_at + 1] and "Task: t7 · running" in active, active
assert {"y": top_at + 1, "x1": 1, "x2": 120, "action": ("tab", "plan")} in banner_nav["hits"]
for order_width, order_height in ((100, 30), (100, 40), (160, 60)):
    order_rows = graph_text.render_graph(words, order_width, order_height).splitlines()
    first_main = next(i for i, line in enumerate(order_rows) if "MAIN " in line)
    assert order_rows[0].startswith("OMS · ") and "Main" in order_rows[1] and order_rows[2].startswith("╭─ Goal") and first_main > 2, order_rows
    assert "week" in order_rows[-2] and "used" not in order_rows[1], order_rows
    for provider in ("claude", "codex"):
        one = deepcopy(words)
        one["provider_status"] = {provider: {"weekly_used": 41, "context_left": 70}}
        quota_row = graph_text.render_graph(one, order_width, order_height).splitlines()[-2]
        assert "%s week 59%% left" % provider in quota_row, quota_row
        narrow_row = usage_line(one, 36, True)
        assert "%s W 59%% left" % provider.title() in narrow_row and "C 30% used" in narrow_row, narrow_row
long_goal = deepcopy(words)
long_goal["plan"]["goal"] = "Ship the shared plan board with every main claiming work from it " * 2
wrapped_top = graph_text.render_graph(long_goal, 80, 40).splitlines()
assert wrapped_top[2].startswith("╭─ Goal") and wrapped_top[3].startswith("│ Ship the shared"), wrapped_top
assert any(line.startswith("╰") for line in wrapped_top[4:8]), wrapped_top
full_goal_rows = [text for text, _ in graph_text.goal_box(long_goal, 80, True, False)]
assert "6/9 verified" in full_goal_rows[0] and len(full_goal_rows) > 3 and all(display_width(line) == 80 for line in full_goal_rows), full_goal_rows
assert "claiming work from it" in " ".join(full_goal_rows), full_goal_rows
plain_goal = graph_text.render_graph(words, 100, 40, unicode=False).splitlines()[2]
assert plain_goal.startswith("+- Goal") and "6/9 verified" in plain_goal and plain_goal.isascii(), plain_goal
cancelled_goal = deepcopy(words)
cancelled_goal["plan"]["by_state"]["cancelled"] = 2
assert "cancelled" not in graph_text.goal_banner(cancelled_goal, 120, True)[0][0]
assert "6/7 verified" in graph_text.goal_banner(cancelled_goal, 120, True)[0][0]
finished_goal = deepcopy(words)
finished_goal["plan"].update(task_count=9, idle_days=44, by_state={"done": 6, "cancelled": 3})
wide_finished = graph_text.goal_banner(finished_goal, 120, True)[0][0]
narrow_finished = graph_text.goal_banner(finished_goal, 42, True)[0][0]
wide_finished_text = " ".join(line for line, _ in graph_text.goal_banner(finished_goal, 120, True))
assert "Goal finished 6/6 verified · 3 cancelled" in wide_finished, wide_finished
assert "superseded (instant)" in wide_finished_text and "completed-external (re-runs acceptance ~9 min)" in wide_finished_text, wide_finished_text
assert "oms agent-plan retire --check" in wide_finished_text, wide_finished_text
assert len(graph_text.goal_banner(finished_goal, 120, True)) <= 4, "a finished goal banner must stay compact"
assert "Goal finished 6/6" in narrow_finished and "—" not in narrow_finished and "Ship the shared plan board" not in narrow_finished, narrow_finished
finished_box = graph_text.goal_box(finished_goal, 80, True)
# The board's goal box names the retire choices too; narrow boxes keep the single line.
assert finished_box[1][1] == "metadata" and "Goal finished" in finished_box[1][0], finished_box
box_text = " ".join(text for text, _ in finished_box)
assert "superseded (instant)" in box_text and "completed-external" in box_text and "retire --check" in box_text, finished_box
for columns in (28, 42, 80, 120):
    for glyphs in (True, False):
        finished_rows = graph_text.goal_box(finished_goal, columns, glyphs)
        assert (len(finished_rows) == 3) == (columns < 46) and len(finished_rows) <= 5, finished_rows
        assert all(display_width(text) == columns for text, _ in finished_rows), finished_rows
        if columns == 28:
            assert "Finished 6/6" in finished_rows[1][0] and "..." not in finished_rows[1][0], finished_rows
# Cancelled tasks are closed, never verified: they leave the total. A running task whose calls all ended is stale,
# one whose call awaits its main's decision stays claimed ("in review" is the plan's review state only), and the
# main's card counts neither as running.
held_flow = deepcopy(single_flow)
held_calls = {"researcher": ("done", "t-ended"), "explorer": ("review", "t-review"), "builder": ("working", "t-live")}
for attempt in held_flow["attempts"]["active_recent"]:
    if attempt["attempt_id"] in held_calls:
        attempt["state"], attempt["task_id"] = held_calls[attempt["attempt_id"]]
held_tasks = [{"id": "t-d%s" % n, "title": "Done", "state": "done"} for n in range(4)] + [
    {"id": "t-c%s" % n, "title": "Dropped", "state": "cancelled"} for n in range(3)] + [
    {"id": ident, "title": "Held " + ident, "state": "running", "claimed_by": "flow-main",
     "claimed_by_participant": "flow-main", "claimed_at": "2026-10-11T12:0%s:00Z" % n}
    for n, ident in enumerate(("t-ended", "t-review", "t-live"))]
held_flow["plan"] = {"present": True, "goal": "Count held work honestly", "task_count": len(held_tasks), "idle_days": 0,
                     "by_state": {"done": 4, "cancelled": 3, "running": 3}, "tasks": held_tasks}
held_flow["room"]["repo_tasks"] = held_tasks
assert graph_text.held_split(held_flow) == ({"t-review"}, {"t-ended"})
# Lease lineage survives claim refreshes: an old ended call cannot make a touched claim look fresh,
# a matching live call still holds it, and a re-claimed lease rejects the prior call.
lease_flow = deepcopy(held_flow)
lease_tasks = {task["id"]: task for task in lease_flow["plan"]["tasks"]}
lease_attempts = {row["attempt_id"]: row for row in lease_flow["attempts"]["active_recent"]}
lease_tasks["t-ended"].update(lease_id="lease-current-ended", claimed_at="2026-10-12T00:00:00Z")
lease_attempts["researcher"].setdefault("panel", {})["lease_id"] = "lease-current-ended"
lease_tasks["t-live"]["lease_id"] = "lease-current-live"
lease_attempts["builder"].setdefault("panel", {})["lease_id"] = "lease-current-live"
lease_tasks["t-review"].update(lease_id="lease-reclaimed", claimed_at="2026-10-12T00:00:00Z")
lease_attempts["explorer"].setdefault("panel", {})["lease_id"] = "lease-previous"
# A re-claimed lease with no call carrying it has no evidence the work stopped: it stays claimed.
assert graph_text.held_split(lease_flow) == (set(), {"t-ended"}), graph_text.held_split(lease_flow)
no_call_flow = deepcopy(lease_flow)
no_call_flow["attempts"]["active_recent"] = [row for row in no_call_flow["attempts"]["active_recent"]
                                             if row.get("task_id") != "t-ended"]
assert "t-ended" not in graph_text.held_split(no_call_flow)[1], graph_text.held_split(no_call_flow)
assert graph_text.plan_summary(held_flow)[1:] == (4, 7, 0, 2, 1), graph_text.plan_summary(held_flow)
held_wide = graph_text.goal_box(held_flow, 127, True)[0][0]
assert "4/7 verified / 2 claimed / 1 stale" in held_wide and display_width(held_wide) == 127, held_wide
for columns in (28, 42, 57, 80, 127):
    for glyphs in (True, False):
        held_box = [text for text, _ in graph_text.goal_box(held_flow, columns, glyphs)]
        assert all(display_width(text) == columns for text in held_box), held_box
        held_note = " ".join(text[2:-2].strip() for text in held_box[2:-1])
        assert "1 stale, not counted: no live worker" in held_note and "oms agent-plan release --id ID" in held_note, held_box
        assert "--include-running" not in "\n".join(held_box)
held_rows = [text for text, unused in graph_text.plan_rows(held_flow, {}, 120, True)]
assert any("t-ended · running (worker ended) · Held t-ended" in text for text in held_rows), held_rows
assert any("t-review · running (awaiting decision) · Held t-review" in text for text in held_rows), held_rows
assert any("t-live · running · Held t-live" in text for text in held_rows), held_rows
held_card = graph_text.render_graph(held_flow, 127, 50, navigation={"dismissed": True})
assert "Task: t-live · running · 1 awaiting decision" in held_card and "more running" not in held_card, held_card
# A held task in the plan's review state reads "in review" on the card, as in the goal box and the Plan tab.
review_hold = deepcopy(held_flow)
next(t for t in review_hold["plan"]["tasks"] if t["id"] == "t-live")["state"] = "review"
review_hold["plan"]["by_state"] = {"done": 4, "cancelled": 3, "running": 2, "review": 1}
review_card = graph_text.render_graph(review_hold, 127, 50, navigation={"dismissed": True})
assert "Task: t-live · in review · 1 awaiting decision" in review_card and "4/7 verified / 1 in review" in review_card, review_card
everything_ended = deepcopy(held_flow)
for attempt in everything_ended["attempts"]["active_recent"]:
    if attempt["attempt_id"] in held_calls:
        attempt["state"] = "done"
assert "Task: t-live · worker ended" in graph_text.render_graph(everything_ended, 127, 50, navigation={"dismissed": True})
assert graph_text.plan_summary(everything_ended)[1:] == (4, 7, 0, 0, 3)
# A finished call awaiting admission or a blocked call awaits; an unknown call keeps its task claimed; the plan's
# own stale claims and reviews join the stale count once, apart from the ended and awaiting tasks.
pending_holds = deepcopy(held_flow)
pending_holds["awaiting_admission"] = ["researcher"]
pending_holds["attempts"]["active_recent"] = [a for a in pending_holds["attempts"]["active_recent"] if a["attempt_id"] != "builder"]
pending_holds["room_attempts"] = {"builder": {"attempt_id": "builder", "task_id": "t-live"}}
next(a for a in pending_holds["attempts"]["active_recent"] if a["attempt_id"] == "explorer")["state"] = "blocked"
assert next(m for m in graph_text.nodes(pending_holds) if m["participant"] == "builder")["state"] == "presence unknown"
assert graph_text.held_split(pending_holds) == ({"t-ended", "t-review"}, set())
pending_holds["plan"].update(task_count=13, stale_claims=1, stale_reviews=1,
                             by_state={"done": 4, "cancelled": 3, "running": 3, "claimed": 1, "review": 2})
assert graph_text.plan_summary(pending_holds)[1:] == (4, 10, 1, 3, 2), graph_text.plan_summary(pending_holds)
# Only the current holder's calls speak for a hold: a call another main left, or one the claiming main started
# before this claim, is not evidence; a call that holds the claim itself always is.
holder = deepcopy(everything_ended)
holder["room"]["participants"].append(next(m for m in flow_members if m["participant"] == "flow-other"))
holder["attempts"]["active_recent"].append(next(a for a in flow["attempts"]["active_recent"] if a["attempt_id"] == "flow-other"))
holder_tasks = {t["id"]: t for t in holder["plan"]["tasks"]}
holder_tasks["t-review"].update(claimed_by="flow-other", claimed_by_participant="flow-other")
holder_tasks["t-live"].update(claimed_by="builder", claimed_by_participant="builder")
for ident in ("researcher", "builder"):
    next(m for m in holder["room"]["participants"] if m["participant"] == ident)["joined_at"] = "2026-10-11T11:00:00Z"
assert graph_text.held_split(holder) == (set(), {"t-live"}), graph_text.held_split(holder)
assert graph_text.plan_summary(holder)[1:] == (4, 7, 0, 2, 1), graph_text.plan_summary(holder)
holder_rows = [text for text, unused in graph_text.plan_rows(holder, {}, 120, True)]
assert any("t-review · running · Held t-review" in text for text in holder_rows), holder_rows
assert any("t-ended · running · Held t-ended" in text for text in holder_rows), holder_rows
holder_card = graph_text.render_graph(holder, 157, 60, navigation={"dismissed": True})
assert "Task: t-ended · running" in holder_card and "worker ended" not in holder_card, holder_card
for columns, rows_high, surface in ((36, 50, "tree"), (50, 50, "tree"), (80, 50, "graph"), (127, 40, "graph"), (127, 90, "graph")):
    nav = {}
    shown = render(geometry_flow, "codex", columns, rows_high, view="graph", navigation=nav, main_attempt="flow-attempt")
    assert nav["surface"] == surface and nav["tab_strip_visible"], (columns, rows_high, nav)
    assert any(h["action"] == ("tab", "messages") for h in nav["hits"]), nav
    assert "Attention" in "\n".join(shown.splitlines()[:10]), shown
    assert "needs" in nav["visible_tabs"], nav
    if (columns, rows_high) == (127, 90):
        upper_end = max(b["y2"] for b in nav["bands"] if b["name"] != "detail")
        assert upper_end < nav["box"]["y1"] and nav["box"]["y2"] == rows_high - 2, shown
        assert nav["box"]["y2"] - nav["box"]["y1"] - 1 >= min(dock_body_rows(rows_high), max(3, nav["dock_content_rows"])), nav
many_needs = deepcopy(geometry_flow)
for attempt in many_needs["attempts"]["active_recent"]:
    if attempt["panel"]["role"] != "main":
        attempt["state"] = "waiting_approval"
attention_rows = needs_strip(many_needs, 36, True)
# The counts header opens the Attention tab and replaces the "+N more" row, so the strip still holds four rows.
assert len(attention_rows) == 1 and attention_rows[0]["action"] == ("tab", "needs")
assert attention_rows[0]["text"].startswith("Attention %s" % len(inbox_items(many_needs))), attention_rows
# Pinned attention follows header/navigation order in both the keyboard list and the hit map.
many_needs["room"]["repo_tasks"] = [{"id": "last-task", "title": "Last task", "state": "ready"}]
needs_nav = {}
needs_picture = render(many_needs, "codex", 50, 50, view="tree", navigation=needs_nav)
pinned_actions = [item["action"] for item in attention_rows]
hit_actions = [hit["action"] for hit in sorted(needs_nav["hits"], key=lambda hit: hit["y"])]
assert hit_actions.index(pinned_actions[0]) < hit_actions.index(("task", "last-task")), needs_picture
choose(("end",), needs_nav)
assert needs_nav["selected"] == ("task", "last-task"), needs_nav
repeated_needs = deepcopy(many_needs)
repeated_needs["room"]["participants"].append(deepcopy(next(
    member for member in repeated_needs["room"]["participants"] if member["participant"] == "builder")))
assert needs_strip(repeated_needs, 36, True) == attention_rows, "duplicate call consumed a pinned row"
plain_needs = deepcopy(many_needs)
for member in plain_needs["room"]["participants"]:
    member["label"] = "plain label"
same_labels = needs_strip(plain_needs, 200, True, limit=100)
assert len(same_labels) == 1 and same_labels[0]["action"] == ("tab", "needs")
assert {item["action"] for item in attention_items(plain_needs)} == {item["action"] for item in inbox_items(plain_needs)}, "identical labels remain reachable in Attention"
# The Attention queue ranks by urgency, not recency: needs you, failed, timed out, decide (oldest first), stale, mail, info;
# within any other class the newest comes first. A superseded failure or one whose task closed leaves the queue, and
# a held task whose failed call is listed is not repeated as stale.
from panel_view import attention_items, attention_header
from unittest.mock import patch
import panel_view
queue = deepcopy(held_flow)
for attempt in queue["attempts"]["active_recent"]:
    state, age = {"builder": ("waiting_approval", 1), "advisor": ("waiting_input", 9), "explorer": ("review", 5),
                  "reviewer": ("done", 30), "researcher": ("done", 40)}.get(attempt["attempt_id"], (attempt["state"], 0))
    attempt.update(state=state, updated_at=minutes_ago(age))
queue["awaiting_admission"] = ["reviewer"]
queue["plan"]["tasks"].append({"id": "t-crash", "title": "Held t-crash", "state": "running", "claimed_by": "flow-main",
                               "claimed_by_participant": "flow-main", "claimed_at": "2026-10-11T12:09:00Z"})
for seq, (ident, state, age, task) in enumerate((("crash-new", "failed", 2, "t-open"), ("crash-held", "failed", 6, "t-crash"),
        ("crash-old", "timed_out", 20, "t-open2"), ("crash-redo", "failed", 3, "t-redo"), ("retry-redo", "working", 1, "t-redo"),
        ("crash-closed", "failed", 4, "t-d0"))):
    queue["room"]["participants"].append(dict(next(m for m in queue["room"]["participants"] if m["participant"] == "researcher"),
                                              participant=ident, seq=60 + seq, label="Crash " + ident))
    queue["attempts"]["active_recent"].append({"attempt_id": ident, "state": state, "updated_at": minutes_ago(age), "task_id": task,
        "panel": {"room_id": "flow-room", "room_participant": ident, "role": "worker"}})
queue["room"]["messages"] = [
    {"id": "note-all", "sender": "flow-main", "recipient": "all", "message_kind": "note", "text": "Status for all",
     "ts": minutes_ago(7), "pending_for": ["advisor"]},
    {"id": "ask-all", "sender": "flow-main", "recipient": "all", "message_kind": "question", "text": "Which way?",
     "ts": minutes_ago(50), "pending_for": ["advisor"]}]
queue["room"]["publications"] = {"wake": {"status": "failed", "ts": minutes_ago(8)}}
assert graph_text.held_split(queue)[1] == {"t-ended", "t-crash"}, graph_text.held_split(queue)
ranked = attention_items(queue)
assert [item["row_id"][1] for item in ranked] == ["builder", "advisor", "ask-all", "crash-new", "crash-held", "crash-old",
                                                  "reviewer", "explorer", "t-ended", "note-all", "wake-up"], ranked
assert [item["cls"] for item in ranked] == ["needs"] * 3 + ["failed"] * 2 + ["timed_out"] + ["decide"] * 2 + ["stale", "mail", "info"]
assert [item["age"] for item in ranked[6:8]] == ["30m", "5m"] and not any(item["age"] for item in ranked[:6] + ranked[8:])
assert ranked[8]["action"] == ("tab", "plan") and "Held t-ended" in ranked[8]["what"], ranked[8]
# Counts drop zero classes; narrower widths shorten the class words, then keep only the total.
full_header = "Attention 11 · 3 needs you · 2 failed · 1 timed out · 2 decide · 1 stale · 1 mail · 1 info"
short_header = "Attention 11 · 3 you · 2 fail · 1 timeout · 2 dec · 1 stale · 1 mail · 1 info"
assert attention_header(ranked, 127, True) == full_header
assert attention_header(ranked, len(full_header) - 1, True) == short_header
assert attention_header(ranked, len(short_header) - 1, True) == "Attention 11"
assert attention_header(ranked, 11, True) == "Attn 11" and attention_header(ranked, 3, True) == "At…"
assert attention_header(ranked, 127, False) == full_header.replace(" · ", " / ")
assert attention_header(attention_items(held_flow), 80, True) == "Attention 2 · 1 decide · 1 stale"
strip = needs_strip(queue, 127, True)
assert [row["action"] for row in strip] == [("tab", "needs")], strip
assert strip[0]["text"] == full_header, strip
assert needs_strip(queue, 127, True, ranked=ranked) == strip
# The compact board repeats no ranked-item previews; Attention owns the full queue.
queue_screen = graph_text.render_graph(queue, 127, 40, navigation={"dismissed": True})
assert sum(line.startswith("Attention 11") for line in queue_screen.splitlines()) == 1, queue_screen
# Repeated reviews collapse under their task and expand through the existing Past input path.
review_group = [dict(review_member, participant="review-%s" % n, task_id="same-task") for n in range(3)]
review_group_nav = {"tab": "past"}
folded_reviews = graph_text.past_body(review_group, 100, 8, review_group_nav, {}, True)
assert len(folded_reviews) == 2 and "same-task · 3 reviews" in folded_reviews[1][0], folded_reviews
review_group_nav["selected"] = ("past-group", "same-task")
assert graph_text.past_event(("enter",), review_group_nav)
expanded_reviews = graph_text.past_body(review_group, 100, 8, review_group_nav, {}, True)
assert len(expanded_reviews) == 5 and review_group_nav["past_ids"][1:] == [("result", "review-%s" % n) for n in range(3)], expanded_reviews
assert graph_text.past_event(("enter",), review_group_nav)
assert len(graph_text.past_body(review_group, 100, 8, review_group_nav, {}, True)) == 2
# The Attention tab groups the queue under one heading per class, in rank order, rows as in the strip.
group_nav = {}
group_rows = graph_text.attention_body(ranked, 123, 30, group_nav, True)
assert group_rows[0][0] == full_header + " · Enter finalize", group_rows[0]
assert len(group_rows) == len(ranked) + 1, group_rows
# attention-classes: each owner action is named by its real class.
class_rows = {item["cls"]: group_rows[i + 1][0] for i, item in enumerate(ranked) if item["cls"] in {"needs", "decide", "stale", "mail", "info"}}
assert "answer needed" in class_rows["needs"]
assert "awaiting decision" in class_rows["decide"]
assert "held, workers ended" in class_rows["stale"]
assert "unread mail" in class_rows["mail"]
assert "delivery failed" in class_rows["info"]
assert [row[1] for row in group_rows if row[1]] == [item["action"] for item in ranked if item["action"]]
selectable = [item for item in ranked if item["action"]]
assert group_nav["needs_ids"] == [item["action"] for item in selectable] and \
    group_nav["needs_keys"] == [item["row_id"] for item in selectable], group_nav
# Rows are picked per item: the main's question and its note share an action but light up and step one at a time.
def picked_rows():
    return [row[0].strip() for row in graph_text.attention_body(ranked, 123, 30, group_nav, True) if row[2]]
group_nav["selected"] = ("chat", "flow-main")
assert picked_rows() == ["answer needed · answer request · to all: Which way?"], picked_rows()
stops = []
for unused in range(8):
    assert graph_text.past_event(("down",), group_nav, "needs")
    stops.append(group_nav["needs_pick"])
assert stops == [item["row_id"] for item in selectable[3:]] + [selectable[-1]["row_id"]], stops
assert group_nav["selected"] == ("chat", "flow-main") and picked_rows() == ["unread mail · read message · to all: Status for all"]
assert graph_text.past_event(("up",), group_nav, "needs") and group_nav["selected"] == ("tab", "plan") and \
    picked_rows() == ["held, workers ended · t-ended · release or continue task"], picked_rows()
for columns, rows_high in ((127, 40), (57, 40)):
    tab_nav = {"tab": "needs", "dismissed": True}
    tab_picture = render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav, main_attempt="flow-attempt")
    assert tab_nav["tab_body_visible"] and any(label in tab_picture for label in ("Attention 11", "Attn 11")), tab_picture
    assert "Enter finalize" in tab_picture and "answer request" in tab_picture, tab_picture
    # Arrows pick a row, Enter shows it in Detail; End reaches the last mail, the stale hold above it opens the Plan tab.
    assert panel.tab_event(("home",), tab_nav) and tab_nav["selected"] == ("result", "builder"), tab_nav
    assert panel.tab_event(("enter",), tab_nav) and tab_nav["tab"] == "needs" and \
        tab_nav["finalize_call"]["target"] == ("result", "builder"), tab_nav
    shown = render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav, main_attempt="flow-attempt")
    assert "Enter finalize" in "\n".join(shown.splitlines()[tab_nav["box"]["y1"] - 1:]), shown
    tab_nav["tab"] = "needs"
    render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav, main_attempt="flow-attempt")
    assert panel.tab_event(("end",), tab_nav) and tab_nav["needs_pick"] == ("message", "note-all"), tab_nav
    assert panel.tab_event(("up",), tab_nav) and tab_nav["selected"] == ("tab", "plan"), tab_nav
    assert "Enter Finalize" in render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav,
                                  main_attempt="flow-attempt").splitlines()[-1]
    assert tab_nav["needs_view"]["scroll"] > 0 or columns == 127, tab_nav["needs_view"]
    assert panel.tab_event(("enter",), tab_nav) and tab_nav["finalize_call"]["task_id"] == "t-ended", tab_nav
    # A click on a queue row shows that call in Detail, and f opens its full result.
    tab_nav["tab"] = "needs"
    tab_nav["needs_view"]["scroll"] = 0
    render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav, main_attempt="flow-attempt")
    hit = next(h for h in tab_nav["hits"] if h.get("needs") and h["action"] == ("result", "advisor"))
    assert panel.tab_event(("click", hit["x1"], hit["y"]), tab_nav) and tab_nav["tab"] == "detail" and \
        tab_nav["preview"] == {"target": ("result", "advisor")} and "finalize_call" not in tab_nav, tab_nav
    # The footer names the tab's own keys; f is offered and works only on a call, not on a main's row.
    tab_nav["tab"] = "needs"
    footer = render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav,
                    main_attempt="flow-attempt").splitlines()[-1]
    assert "Enter Detail" in footer and "f Full result" in footer and "Enter Show" not in footer, footer
    assert panel.tab_event(("home",), tab_nav) and panel.tab_event(("f",), tab_nav) and \
        tab_nav["open_call"] == ("result", "builder") and tab_nav["tab"] == "detail", tab_nav
    tab_nav.update(tab="needs", selected=("chat", "flow-main"))
    tab_nav.pop("open_call")
    footer = render(queue, "codex", columns, rows_high, view="graph", navigation=tab_nav,
                    main_attempt="flow-attempt").splitlines()[-1]
    assert "Enter Detail" in footer and "f Full" not in footer and "Enter Chat" not in footer, footer
    assert panel.tab_event(("f",), tab_nav) and "open_call" not in tab_nav, tab_nav
# Without color only the picked row is marked, also when the row above it opens the same chat.
asked = deepcopy(queue)
asked["room"]["messages"].append(dict(asked["room"]["messages"][1], id="ask-all2", text="Which file?"))
asked_ranked = attention_items(asked)
pair = [item["row_id"] for item in asked_ranked if item["action"] == ("chat", "flow-main") and item["cls"] == "needs"]
assert len(pair) == 2, asked_ranked
plain_nav = {"tab": "needs", "dismissed": True, "selected": ("chat", "flow-main"), "needs_pick": pair[1]}
plain = render(asked, "codex", 127, 40, view="graph", navigation=plain_nav, main_attempt="flow-attempt").splitlines()
marked = [line for line in plain[plain_nav["box"]["y1"]:] if line.rstrip("│ ").endswith(" <")]
assert len(marked) == 1 and "Which file?" in marked[0] and any("Which way?" in line for line in plain), plain
# The Attention tab gets the reading dock a Past list gets, also on a 24-row tree.
for columns, rows_high in ((28, 16), (42, 24), (57, 24), (80, 24), (110, 24)):
    dock_nav = {"tab": "needs"}
    render(queue, "codex", columns, rows_high, view="tree", navigation=dock_nav, main_attempt="flow-attempt")
    assert dock_nav["tab_body_visible"] and dock_nav["box"], (columns, rows_high, dock_nav.get("box"))
# One ranked list per frame: the strip, the spare-row summary and the tab share a single inbox computation.
real_inbox = panel_view.inbox_items
for fixture, columns, rows_high, mode in ((summary_room, 127, 40, "graph"), (queue, 127, 40, "graph"),
                                          (queue, 42, 40, "tree")):
    calls_made = []
    counted = lambda *args, **kwargs: calls_made.append(1) or real_inbox(*args, **kwargs)
    with patch.object(panel_view, "inbox_items", counted):
        frame_nav = {"tab": "needs"}
        frame = render(fixture, "codex", columns, rows_high, view=mode, navigation=frame_nav)
    assert len(calls_made) == 1 and frame_nav["tab_body_visible"], (fixture is queue, mode, len(calls_made))
    if fixture is summary_room:
        assert "Attention 1" in frame and "In progress" not in frame, frame
# Every size fits its width exactly as measured, in both glyph sets, on the board and in the Attention tab.
for columns, rows_high in ((28, 16), (42, 24), (57, 24), (80, 24), (110, 24), (127, 40)):
    for glyphs in (True, False):
        for tab in ("detail", "needs"):
            fit_nav = {"tab": tab}
            fitted = render(queue, "codex", columns, rows_high, unicode=glyphs, navigation=fit_nav).splitlines()
            assert len(fitted) <= rows_high and all(display_width(line) <= columns for line in fitted), (columns, fitted)
            assert glyphs or not set("·…⚑◇○»│╭╮╰╯─").intersection("".join(fitted)), fitted
            assert any(line.startswith(("Attention 11", "Attn 11")) for line in fitted), (columns, rows_high, fitted)
decoder = TerminalInput()
assert decoder.decode(b"\033[<0;8;") == []
assert decoder.decode(b"7M\033[<0;8;7m\033[<65;8;7M\033[A\r") == [
    ("click", 8, 7), ("scroll", 3, 8, 7), ("up",), ("enter",)]
assert decoder.decode(b"\033[<32;1;1M") == []
assert percent(True) is None and percent(float("nan")) is None and percent(10**1000) is None
assert percent(0) == 0 and percent(100) == 100
quota_home = temporary / "codex-quota-home"
quota_day = quota_home / "sessions" / "2026" / "10" / "13"
quota_day.mkdir(parents=True)
quota_log = quota_day / "rollout-quota.jsonl"
quota_now = time.time()
quota_limits = {"primary": {"used_percent": 9.0, "window_minutes": 10080,
                            "resets_at": quota_now + 60},
                "secondary": {"used_percent": 70, "window_minutes": 300,
                              "resets_at": quota_now + 60}}
with patch.dict(os.environ, {"CODEX_HOME": str(quota_home)}):
    assert codex_weekly_reading(quota_now) == {}
    for weekly_slot in ("primary", "secondary"):
        limits = quota_limits if weekly_slot == "primary" else {
            "primary": quota_limits["secondary"], "secondary": quota_limits["primary"]}
        quota_log.write_text(json.dumps({"rate_limits": dict(quota_limits,
            primary=dict(quota_limits["primary"], used_percent=80))}) + "\n" +
            json.dumps({"payload": {"type": "token_count", "rate_limits": limits}}) + "\n" +
            json.dumps({"payload": {"type": "other"}}) + "\n")
        assert codex_weekly_reading(quota_now)["weekly_used"] == 9
    with patch("panel_metrics.os.open", side_effect=AssertionError("unchanged log was reread")):
        assert codex_weekly_reading(quota_now)["weekly_used"] == 9
        assert codex_weekly_reading(quota_now + 61) == {}, "cache retained an expired quota"
    # A Claude main with no Codex pane still supplies the board and routing with quota.
    quota_report = {"room": {"id": "quota-room", "participants": [{"participant": "quota-main",
        "role": "main", "provider": "claude", "joined": True}]}}
    with patch("panel_chats.windows", return_value={}), patch("panel_chats.current_models", return_value={}):
        quota_status = collect(quota_report, now=quota_now)
    assert quota_status["codex"]["weekly_used"] == 9
    assert "Codex week 91% left" in usage_line({"provider_status": quota_status}, width=200)
    quota_status["claude"]["weekly_used"] = 60
    assert allocate("claude", installed=both, provider_status=quota_status)["provider"] == "codex"
    quota_main = {"room": {"id": "quota-room", "participants": [{"participant": "quota-codex",
        "role": "main", "provider": "codex", "joined": True}]}}
    for visible, expected in (({"weekly_used": 39, "context_left": 83, "source": "TUI"}, 39),
                              ({"weekly_used": None, "context_left": 83, "source": "TUI"}, 9)):
        with patch("panel_chats.windows", return_value={"quota-codex": [{"pane": "%42"}]}), \
                patch("panel_chats.current_models", return_value={}), \
                patch("panel_metrics.pane_reading", return_value=visible):
            live_status = collect(quota_main, now=quota_now, repo=project)["codex"]
        assert live_status["weekly_used"] == expected and live_status["context_left"] == 83
    quota_log.write_text(json.dumps({"rate_limits": dict(quota_limits,
        primary=dict(quota_limits["primary"], resets_at=quota_now - 1))}) + "\n")
    assert codex_weekly_reading(quota_now) == {}
    quota_log.write_text('{"rate_limits": malformed}\n')
    assert codex_weekly_reading(quota_now) == {}
    quota_log.write_text(json.dumps({"rate_limits": quota_limits}) + "\n")
    os.utime(quota_log, (quota_now - 604801, quota_now - 604801))
    assert codex_weekly_reading(quota_now) == {}, "old log supplied weekly quota"
    # The newest file wins, and a quota outside the bounded tail stays unknown.
    newest_log = quota_day / "rollout-newest.jsonl"
    newest_log.write_text(json.dumps({"rate_limits": quota_limits}) + "\n")
    assert codex_weekly_reading(quota_now)["weekly_used"] == 9
    newest_log.write_text(json.dumps({"rate_limits": quota_limits}) + "\n" + "x" * (256 * 1024) + "\n")
    assert codex_weekly_reading(quota_now) == {}
    older_day = quota_day.parent / "10"
    older_day.mkdir()
    (older_day / "rollout-older.jsonl").write_text(json.dumps({"rate_limits": quota_limits}) + "\n")
    for day in ("11", "12"):
        (quota_day.parent / day).mkdir()
    assert codex_weekly_reading(quota_now) == {}, "searched beyond the newest three day directories"
    with patch("panel_metrics.os.scandir", side_effect=OSError("unavailable sessions")):
        assert codex_weekly_reading(quota_now) == {}
    with patch.dict(os.environ, {"CODEX_HOME": ""}), \
            patch("panel_metrics.Path.home", side_effect=RuntimeError("home unavailable")):
        assert codex_weekly_reading(quota_now) == {}
hud = temporary / "panel-hud"
hud.mkdir()
consumer = "a" * 32
cache = hud / ("ctx-" + consumer[:24] + ".json")
cache.write_text(json.dumps({"schema": 1, "used_percentage": 25, "ts": 1000,
                            "seven_day": {"used_percentage": 0, "resets_at": 2000}}))
with patch.dict(os.environ, {"OMS_HUD_CACHE_DIR": str(hud)}):
    assert cached(consumer, 1000)["context_left"] == 75
    assert cached(consumer, 1000)["weekly_used"] == 0
    assert cached(consumer, 1601) == {"source": "cache stale"}
    assert cached(consumer, 999) == {}
    cache.write_text(json.dumps({"used_percentage": True, "ts": 1000,
        "seven_day": {"used_percentage": 50, "resets_at": 900}}))
    assert cached(consumer, 1000)["context_left"] is None and cached(consumer, 1000)["weekly_used"] is None
    cache.unlink()
    if os.name != "nt":
        cache.symlink_to(project / "value.txt")
        assert cached(consumer, 1000) == {}
with patch("panel_chats.windows", return_value={}), patch("panel_chats.native_index", side_effect=AssertionError("render scan")):
    # Earlier mains stay on record; the reading belongs to the live, then newest, one.
    reading = collect(tree_room)
    assert reading["codex"]["participant"] == next(m["participant"] for m in graph_text.nodes(tree_room)
                                                   if m.get("role") == "main" and m.get("provider") == "codex"), reading
    reading = collect(tree_room, main_attempt=main_attempts["codex"])
    assert reading["codex"]["participant"] == main_attempts["codex"], reading
pi_report = {"room": {"id": "pi-room", "participants": [{"participant": "pi-main", "provider": "codex",
    "role": "main", "joined": True, "model": "gpt-6.1-sol"}]}, "attempts": {"active_recent": [{
    "attempt_id": "pi-main", "state": "working", "panel": {"role": "main", "harness": "pi", "room_id": "pi-room"}}]}}
pi_fallback = dict(pi_report, room_attempts={"pi-main": pi_report["attempts"]["active_recent"][0]},
                   attempts={"active_recent": [], "recent": []})
from room_view import model_name
for candidate in (pi_report, pi_fallback):
    with patch("panel_chats.windows", return_value={"pi-main": [{"pane": "%42"}]}), \
            patch("panel_chats.current_models", return_value={}), \
            patch("panel_metrics.pane_reading", side_effect=AssertionError("Pi was parsed as native Codex")):
        reading = collect(candidate, repo=project)
        assert reading["codex"]["context_left"] is None and reading["codex"]["weekly_used"] is None
    assert model_name(nodes(candidate)[0]).startswith("Pi / ")
with patch("panel_metrics.subprocess.run", side_effect=[
        subprocess.CompletedProcess([], 0, "99\t0\n"),
        subprocess.CompletedProcess([], 0, "GPT-6-Sol · Context 83% left · weekly 61% left\n")]) as pane_probe:
    assert pane_reading({"pane": "%42"}, "codex") == {"context_left": 83, "weekly_used": 39, "source": "TUI"}
    assert pane_probe.call_args.args[0][-4:] == ["-S", "91", "-E", "98"]
# The smaller task panel preserves frame-local navigation and never upgrades
# native process exit to owner acceptance.
summary_room = deepcopy(tree_room)
# Earlier calls can evict terminal mains from the bounded dashboard history.
summary_main = next(p for p in summary_room["room"]["participants"] if p["role"] == "main" and p["joined"])
summary_room["attempts"]["recent"].append({"attempt_id": "summary-exited-main", "state": "done",
    "panel": {"role": "main", "room_id": shared, "room_participant": summary_main["participant"]}})
for attempt in summary_room["attempts"]["active_recent"] + summary_room["attempts"]["recent"]:
    if attempt.get("panel", {}).get("role") == "main":
        attempt["state"] = "done"
# The room's other joined mains have no attempt record; they ended too, so no main is live.
summary_room["attempts"]["recent"] += [{"attempt_id": "summary-ended-" + m["participant"], "state": "done",
    "panel": {"role": "main", "room_id": shared, "room_participant": m["participant"]}}
    for m in nodes(summary_room) if m.get("role") == "main" and not m.get("attempt")]
for columns, rows in ((32, 28), (80, 24), (2, 8)):
    nav = {}
    compact_panel = render(summary_room, "codex", columns, rows, view="summary", navigation=nav)
    assert len(compact_panel.splitlines()) <= rows
    assert all(display_width(line) <= columns for line in compact_panel.splitlines())
    assert all(hit["y"] <= len(compact_panel.splitlines()) for hit in nav["hits"])
assert "exited" in render(summary_room, "codex", 120, 99, view="summary")
usage_board = deepcopy(summary_room)
usage_board["operations"] = {"recent": [{"kind": "call", "provider": "codex", "event_id": model,
    "served_model": model, "tokens": 10} for model in ("gpt-6-sol", "gpt-6-astra", "gpt-6-luna")]}
small_board = render(usage_board, "codex", 32, 28, view="summary")
expanded_board = render(usage_board, "codex", 100, 28, view="tree")
assert "USAGE" not in small_board and "USAGE" not in expanded_board

# The compact goal banner and short-height fallback keep only the sections that
# fit; usage stays unique, and task titles keep their navigation target while scrolling.
boxed_room = deepcopy(usage_board)
boxed_room["room"]["repo_tasks"] = [{"id": "layout", "state": "ready",
    "title": "Keep a long task title readable when the terminal has fewer columns than its description"}]
for columns, rows in ((108, 36), (100, 28), (42, 24), (32, 28)):
    nav = {}
    screen = render(boxed_room, "codex", columns, rows, view="tree", navigation=nav)
    base_boxes = 2
    expected_boxes = base_boxes + int(bool(nav.get("log_box")))
    assert screen.count("╭") == screen.count("╰") == expected_boxes, screen
    assert "Model use, last 8 calls" not in screen and "USAGE" not in screen, screen
    assert "DECLARED SCOPES" not in screen, screen
    assert len(screen.splitlines()) <= rows and all(display_width(line) <= columns for line in screen.splitlines()), screen
    for key in ("home", "end"):
        boundary = dict(nav)
        choose((key,), boundary)
        assert any(hit["action"] == boundary["selected"] for hit in nav["hits"]), (key, nav)
    choose(("end",), nav)
    assert nav["selected"] == ("task", "layout"), nav
    scrolled = render(boxed_room, "codex", columns, rows, view="tree", navigation=nav)
    assert scrolled.count("╭") == scrolled.count("╰") == base_boxes + int(bool(nav.get("log_box"))), scrolled
    assert any(hit["action"] == ("task", "layout") for hit in nav["hits"]), nav
assert "Claude W 0% left · C 100% used" in render(dict(usage_board, provider_status={"claude": {
    "weekly_used": 100, "context_left": 0}}), "claude", 32, 28, view="summary")
empty_room = dict(usage_board, room={"id": "empty", "participants": []}, attempts={"active_recent": [], "recent": []})
# The control menu is an inbox: empty rooms onboard, failures and waits list under "Needs you", one key acts.
onboarding = render(empty_room, "codex", 100, 28, view="tree", menu=True)
assert "Press 1 (Claude) or 2 (Pi) to start the sole main" in onboarding and "Nothing needs you right now" in onboarding
assert "[q] Detach" in render(empty_room, "codex", 100, 28, view="tree", menu=True, managed=True)
inbox_room = {"repo": {"name": "r"}, "collection": {"ok": True}, "finalized": {}, "awaiting_admission": ["w3"],
    "room": {"id": "inbox-room", "title": "Shared", "participants": [
        {"participant": "m1", "role": "main", "joined": True, "provider": "claude", "model": "claude-opus-5-5", "seq": 1},
        {"participant": "w1", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 2, "parent": "m1"},
        {"participant": "w2", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 3, "parent": "m1"},
        {"participant": "w3", "role": "worker", "joined": True, "provider": "codex", "model": "gpt-6.1-sol", "seq": 4, "parent": "m1-attempt"}],
        "messages": [{"id": "n1", "sender": "m1", "recipient": "all", "message_kind": "question", "text": "Which branch?\x1b[2J",
                      "pending_for": ["w1"], "answered": False, "ts": "2026-10-07T00:00:00Z"}]},
    "attempts": {"available": True, "recent": [], "active_recent": [
        {"attempt_id": "m1-attempt", "state": "working", "panel": {"role": "main", "room_participant": "m1", "room_id": "inbox-room"}},
        {"attempt_id": "a2", "state": "failed", "updated_at": "2026-10-07T00:01:00Z",
         "panel": {"role": "worker", "room_participant": "w1", "room_id": "inbox-room", "label": "Fix parser"}},
        {"attempt_id": "a3", "state": "waiting_approval", "updated_at": "2026-10-07T00:02:00Z",
         "panel": {"role": "worker", "room_participant": "w2", "room_id": "inbox-room", "label": "Run migration"}},
        {"attempt_id": "a4", "state": "done", "updated_at": "2026-10-07T00:00:30Z",
         "panel": {"role": "worker", "access": "write", "room_participant": "w3", "room_id": "inbox-room", "label": "Write patch"}}]}}
def exercise_exited_main_mail():
    from copy import deepcopy
    import json
    from pathlib import Path
    import subprocess
    from types import SimpleNamespace
    from unittest.mock import patch
    import panel_messages
    import room
    import room_view
    import terminal_panel as panel
    from dashboard_projection import display_width
    from panel_view import inbox_items, render

    def main(ident, state="done", provider="claude", created="2026-10-10T00:00:00Z"):
        return {"attempt_id": ident, "tool": "panel-main", "provider": provider, "state": state,
                "terminal": state == "done", "created_at": created, "updated_at": created,
                "refs": {"panel_role": "main", "panel_owner": provider,
                         "panel_room_id": "mail", "panel_room_participant": ident}}

    old, earlier = main("mail-old1"), main("mail-old2")
    successor = main("mail-new1", "working", created="2026-10-11T00:00:00Z")
    ledger = [old, earlier]
    report = {"repo": {"name": "mail"}, "collection": {"ok": True},
              "attempts": {"active_recent": [], "recent": []}, "room": {"id": "mail",
              "participants": [{"participant": r["attempt_id"], "role": "main", "provider": "claude",
                                "model": "claude-opus-5-5", "joined": True, "seq": i}
                               for i, r in enumerate(ledger)] + [
                  {"participant": "mail-reviewer", "role": "reviewer", "provider": "codex", "joined": True,
                   "parent": old["attempt_id"], "model": "gpt-6.1-sol", "seq": 3}],
              "call_results": {"mail-reviewer": {"exit": 0}}, "messages": [], "open_questions": []}}

    def project():
        report["attempts"]["recent"] = [dict(r, panel={"role": "main", "room_id": "mail",
            "room_participant": r["attempt_id"]}) for r in ledger]
        with patch.object(panel, "events", side_effect=AssertionError("succession reread ledger")):
            report["message_successors"] = room_view.message_successors(ledger, room_view.nodes(report))

    for ident, kind, target in (("mail-q", "question", "mail-old1"),
                                ("mail-note", "note", "mail-old1"),
                                ("result-mail-reviewer", "handoff", "mail-old1"),
                                ("mail-other", "question", "mail-old2")):
        report["room"]["messages"].append({"id": ident, "sender": "mail-reviewer", "recipient": target,
            "targets": [target], "message_kind": kind, "pending_for": [target], "answered": False,
            "text": "Call exit=0; verify\x1b[2J\u202e", "ts": "2026-10-10T00:00:00Z"})
    report["room"]["open_questions"] = [{k: m[k] for k in ("id", "sender", "recipient", "ts")}
                                         for m in report["room"]["messages"] if m["message_kind"] == "question"]
    report["room"]["pending_messages"] = deepcopy(report["room"]["messages"])
    report["room"]["message_count"] = 4

    def surfaces(inherited=False):
        before = deepcopy(report)
        members = room_view.nodes(report)
        items = inbox_items(report, False)
        assert len(items) == 4, items
        if inherited:
            assert report["message_successors"] == {"mail-old1": "mail-new1", "mail-old2": "mail-new1"}
            assert all(item["action"] == ("chat", "mail-new1") for item in items), items
            assert all("#new1 (was #old" in item["what"] and "main exited" not in item["what"] for item in items)
            assert room_view.message_pending(report["room"]["messages"][0], report["message_successors"]) == {"mail-new1"}
        else:
            assert not report["message_successors"]
            assert all("(main exited)" in item["what"] for item in items), items
        for glyphs in (True, False):
            nav = {}
            body = panel_messages.tab_body(report, 200, 20, nav, glyphs)
            assert body[0][0] == "Filter: needs you (4)" and len(nav["message_ids"]) == 4, body
            tagged = [line for line, action, _ in body if action and (" ● " in line or " * " in line)]
            assert len(tagged) == room_view.live_unread(report, members, room_view.settler(report)[0]) == 4
            assert all(("(was #old" if inherited else "(main exited)") in line for line in tagged), tagged
            for columns, height in ((50, 18), (127, 40), (200, 60)):
                screen = render(report, "codex", columns, height, unicode=glyphs, menu=True)
                assert len(screen.splitlines()) <= height and all(display_width(line) <= columns for line in screen.splitlines())
                mail_nav = {"tab": "messages"}
                board = render(report, "codex", columns, height, unicode=glyphs, view="tree", navigation=mail_nav)
                assert len(board.splitlines()) <= height and all(display_width(line) <= columns for line in board.splitlines())
                assert len(panel_messages.needs_main(report)) == 2
                assert ("Messages 2" if columns >= 76 else "[Msg 2]") in board, board
                assert "Messages: 4 unread for live participants" in board, board
                if height >= 40:
                    assert "Filter: needs you (4)" in board, board
                assert len(mail_nav["message_ids"]) == 4, mail_nav
                assert "\x1b" not in screen + board and (glyphs or (screen + board).isascii())
        assert report == before, "display changed room evidence"

    project()
    surfaces()
    def history_window():
        rows = [{"thread": "mail", "seq": 0, "room_event": {"kind": "created"}}]
        rows += [{"seq": i + 1, "room_event": dict(kind="join", label=m["participant"], **{k: m[k] for k in
                    ("participant", "provider", "role")})}
                 for i, m in enumerate(report["room"]["participants"])]
        start = len(rows)
        rows += [{"seq": start, "ts": "2026-10-10T00:00:00Z", "text": "Old pending note\x1b[2J\u202e" + "x" * 1000,
                  "room_event": {"kind": "message", "id": "old-note", "sender": "mail-reviewer",
                                 "recipient": "mail-old1", "message_kind": "note"}}]
        rows += [{"seq": start + i + 1, "text": "New status", "room_event": {
                  "kind": "message", "id": "status-%d" % i, "sender": "mail-reviewer",
                  "recipient": "all", "message_kind": "status"}} for i in range(12)]
        def projected():
            with patch.object(room, "records", return_value=rows):
                return dict(report, room=room.status(str(temporary), "mail"))
        window = projected()
        assert len(window["room"]["messages"]) == 12 and all(m["id"] != "old-note" for m in window["room"]["messages"])
        assert len(window["room"]["pending_messages"][0]["text"]) == 300
        before = deepcopy(window)
        items = inbox_items(window, False)
        target = report["message_successors"].get("mail-old1", "mail-old1")
        assert len(items) == 1 and items[0]["action"] == ("chat", target), items
        assert "Old pending note" in items[0]["what"] and "\x1b" not in str(items) and "\u202e" not in str(items)
        assert ("(main exited)" if target == "mail-old1" else "(was #old1)") in items[0]["what"]
        assert room_view.live_unread(window, room_view.nodes(window), room_view.settler(window)[0]) == 1
        screen = render(window, "codex", 127, 40, unicode=False, menu=True)
        assert screen.isascii() and "Needs you / 1" in screen, screen
        assert window == before, "pending projection display changed room evidence"
        # Recent history and pending projection overlap without duplicate rows; the projection is authoritative.
        window["room"]["messages"].append(window["room"]["pending_messages"][0])
        assert len(inbox_items(window, False)) == 1
        window["room"]["pending_messages"] = []
        assert not inbox_items(window, False), "empty projection used stale recent mail"
        rows.append({"seq": start + 13, "room_event": {
            "kind": "received", "participant": "mail-old1", "messages": ["old-note"]}})
        assert not inbox_items(projected(), False), "received note still needs attention"
        rows.pop()
        # Preserve the existing bounded lower-bound count after the 500-message projection cap.
        rows += [{"seq": start + 13 + i, "text": "Pending note", "room_event": {
                  "kind": "message", "id": "extra-%d" % i, "sender": "mail-reviewer",
                  "recipient": "mail-old1", "message_kind": "note"}} for i in range(500)]
        capped = projected()
        assert len(capped["room"]["pending_messages"]) == len(inbox_items(capped, False)) == 500
        assert room_view.live_unread(capped, room_view.nodes(capped), room_view.settler(capped)[0]) == "500+"

    history_window()
    ledger.append(successor)
    report["room"]["participants"].append(dict(report["room"]["participants"][0], participant="mail-new1", seq=4))
    project()
    surfaces(True)
    history_window()
    successor_nav = {"main_attempt": "mail-new1", "messages_view": {
        "filter": 3, "selected": None, "open": None, "scroll": 0}}
    body = panel_messages.tab_body(report, 200, 20, successor_nav, False)
    assert body[0][0] == "Filter: to/from me (4)", body
    old_nav = {"main_attempt": "mail-old1", "messages_view": {
        "filter": 3, "selected": None, "open": None, "scroll": 0}}
    assert panel_messages.tab_body(report, 200, 20, old_nav, False)[0][0] == "Filter: to/from me (0)"
    with patch.object(room, "age", return_value=0):
        assert len(inbox_items(report)) == 4, "an exited main's fresh question vanished"
    # Snapshot reads full main lifecycle evidence outside the dashboard's capped rows.
    with patch.object(panel, "dashboard", side_effect=lambda *args, **kwargs:
                      (dict(deepcopy(report), attempts={"recent": [], "active_recent": []}), 0)), \
            patch.object(room, "status", return_value=deepcopy(report["room"])), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "main_activity", return_value={}), \
            patch.object(panel, "events", side_effect=AssertionError("snapshot reread ledger")), \
            patch.object(panel, "read_shared", side_effect=lambda repo, key, collect, **kw: collect()), \
            patch.object(panel, "awaiting_admission", return_value=[]), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "{}", "")):
        from unittest.mock import Mock
        attempts = Mock(return_value=ledger)
        collected, status = panel.snapshot(Path("."), "mail", _tick=SimpleNamespace(attempts=attempts))
        attempts.assert_called_once_with()
        failed_attempts = Mock(side_effect=ValueError("attempt evidence unavailable"))
        fallback, fallback_status = panel.snapshot(Path("."), "mail", _tick=SimpleNamespace(attempts=failed_attempts))
        failed_attempts.assert_called_once_with()
        assert fallback_status == 0 and not fallback.get("message_successors"), fallback
    assert status == 0 and collected["message_successors"] == report["message_successors"], collected
    assert len(inbox_items(collected)) == 4
    with patch.object(panel, "events", return_value=json.dumps(ledger)):
        assert panel.lineage_ids("unused", "claude", "mail-new1", "mail-new1") == \
               panel.lineage_ids_from_rows(ledger, "claude", "mail-new1", "mail-new1")
    # Unproven, foreign, later and ambiguous predecessors never change recipient ownership.
    for changed in (dict(old, terminal=None), dict(old, provider="codex"),
                    dict(old, created_at="2026-10-12T00:00:00Z"), dict(old, state="working", terminal=False)):
        ledger[0] = changed
        project()
        assert "mail-old1" not in report["message_successors"], (changed, report)
    ledger[0] = old
    ledger.append(dict(successor))
    project()
    assert not report["message_successors"], "duplicate attempt evidence reassigned mail"
    ledger.pop()
    # A resumed participant is already the recipient; normal live-main rows retain their existing presentation.
    ledger[:] = [dict(old, state="working", terminal=False)]
    report["room"]["participants"] = report["room"]["participants"][:1] + [report["room"]["participants"][2]]
    report["room"]["messages"] = report["room"]["messages"][:3]
    report["room"]["open_questions"] = report["room"]["open_questions"][:1]
    project()
    items = inbox_items(report, False)
    assert len(items) == 1 and items[0]["what"] == "to Opus 5.5" and items[0]["action"] == ("chat", "mail-old1"), items
    nav = {}
    body = panel_messages.tab_body(report, 200, 20, nav, False)
    assert body[0][0] == "Filter: needs you (1)" and nav["message_ids"] == ["mail-q"], body
    assert "main exited" not in str(items) + str(body) and "(was #" not in str(items) + str(body)
    print("exited-main mail: markers, succession, unread/filter agreement, bounded safe rendering and live-main invariants passed")

exercise_exited_main_mail()
nav = {}
inbox = render(inbox_room, "codex", 100, 28, menu=True, navigation=nav)
assert "Needs you" in inbox and "Nothing needs you" not in inbox and "Press 1" not in inbox and "START" not in inbox, inbox
from review_backlog import counts as backlog_counts, project as backlog_project, warning as backlog_warning
decision_fixture = deepcopy(inbox_room)
decision_fixture["awaiting_admission"] = []
decision_fixture["plan"] = {"present": True, "goal": "Decisions", "task_count": 2,
                            "by_state": {"running": 2}, "tasks": [
                                {"id": "life-a", "state": "running", "claimed_by_participant": "m1"},
                                {"id": "life-b", "state": "running", "claimed_by_participant": "m1"}]}
for participant, task_id, stamp in (("w1", "life-a", "2026-10-07T00:01:00Z"),
                                    ("w2", "life-b", "2026-10-07T00:02:00Z")):
    decision_call = next(row for row in decision_fixture["attempts"]["active_recent"]
                if row["panel"].get("room_participant") == participant)
    decision_call.update(state="review", task_id=task_id, updated_at=stamp)
assert backlog_counts(decision_fixture) == {"m1": 2}, backlog_counts(decision_fixture)
from panel_view import attention_items
decision_rows = [item for item in attention_items(decision_fixture) if item["cls"] == "decide"]
assert len(decision_rows) == 2 and all(item["age"] for item in decision_rows), decision_rows
from room_view import attention_body, goal_box
strip = attention_body(decision_rows, 120, 20, {}, True)
assert all(any(item["age"] in row[0] for row in strip) for item in decision_rows), strip
# The count lives in the Attention strip ("N decide"); the goal box keeps its plan counts only.
assert "decisions" not in " ".join(row[0] for row in goal_box(decision_fixture, 120, True)), goal_box(decision_fixture, 120, True)
decision_fixture["finalized"]["life-a"] = "rejected"
assert backlog_counts(decision_fixture) == {"m1": 1}, backlog_counts(decision_fixture)
inbox_room["plan"] = {"tasks": [{"id": "review-%s" % n, "state": "review",
                                  "claimed_by_participant": "m1"} for n in range(8)]}
assert backlog_counts(inbox_room) == {"m1": 9}, backlog_counts(inbox_room)
inbox_room["review_backlog"] = backlog_project(inbox_room)
backlog_items = [item for item in inbox_items(inbox_room) if item["row_id"] == ("review-backlog", "m1")]
assert len(backlog_items) == 1 and backlog_items[0]["what"] == backlog_warning(9), backlog_items
# In the Attention queue the backlog warning is a decision and leads that class.
from panel_view import attention_items
backlog_decisions = [item["row_id"] for item in attention_items(inbox_room) if item["cls"] == "decide"]
assert backlog_decisions[:1] == [("review-backlog", "m1")], backlog_decisions
del inbox_room["review_backlog"]
many_patches = deepcopy(inbox_room)
many_patches["plan"] = {"tasks": []}
many_patches["room"] = dict(many_patches["room"], participants=list(many_patches["room"]["participants"]))
many_patches["attempts"] = dict(many_patches["attempts"], active_recent=list(many_patches["attempts"]["active_recent"]))
awaiting_admission = ["w3"]
for number in range(8):
    participant = "pending-%s" % number
    awaiting_admission.append(participant)
    many_patches["room"]["participants"].append({"participant": participant, "role": "worker", "joined": True,
        "provider": "codex", "model": "gpt-6.1-sol", "seq": 10 + number, "parent": "m1-attempt"})
    many_patches["attempts"]["active_recent"].append({"attempt_id": "attempt-%s" % number, "state": "done",
        "panel": {"role": "worker", "access": "write", "room_participant": participant,
                  "room_id": "inbox-room", "label": "Write patch %s" % number}})
many_patches["awaiting_admission"] = awaiting_admission
assert backlog_counts(many_patches) == {"m1": 9}, backlog_counts(many_patches)
assert sum(item["what"].startswith("patch awaits admission") for item in inbox_items(many_patches)) == 9
with patch("review_backlog.counts", wraps=backlog_counts) as backlog_count_reads:
    many_patches["review_backlog"] = backlog_project(many_patches)
    for board_view in ("graph", "tree"):
        # The warning remains in Attention after the pinned strip becomes one row.
        strip = needs_strip(many_patches, 160, True, limit=4)
        from panel_view import attention_items
        assert len(strip) == 1 and "10 decide" in strip[0]["text"] and any(
            "review first" in item["what"] and item["action"] == ("chat", "m1")
            for item in attention_items(many_patches)), strip
        drawing = render(many_patches, "claude", 160, 50, view=board_view, navigation={"tab": "needs"})
        assert "review first" in drawing, (board_view, drawing)
    assert backlog_count_reads.call_count == 1, backlog_count_reads.call_count
for columns, rows in ((50, 18), (80, 40), (127, 40)):
    for menu_report in (inbox_room, dict(inbox_room, room={"id": "broken", "error": "room unreadable: ValueError: bad room"})):
        screen = render(menu_report, "claude", columns, rows, menu=True, managed=True)
        assert "START" not in screen and "Press 1" not in screen, screen
        assert len(screen.splitlines()) <= rows - 1 and all(display_width(line) <= columns for line in screen.splitlines()), screen
        if menu_report["room"].get("error"):
            assert "room unreadable" in screen, screen
    for stale_report, stale_nav in ((empty_room, {"capacity_stale": True}),
                                    (dict(empty_room, capacity_stale=True), {}),
                                    (empty_room, {"room_poll_unknown": True})):
        screen = render(stale_report, "claude", columns, rows, menu=True, managed=True, navigation=stale_nav)
        assert "START" not in screen and "Press 1" not in screen and "[1]" not in screen, screen
        assert stale_nav["main_occupied"], stale_nav
# Default controls and direct graph/tree renders share occupancy and installed harnesses.
import terminal_panel as panel
from panel_view import menu_rows
partial_installed = {"claude", "codex"}
for expanded in (False, True):
    for columns in (57, 40):
        rows = menu_rows(columns, 28, expanded=expanded, installed=partial_installed)
        joined = "\n".join(rows)
        assert "Pi" not in joined, rows
        for key, label in (("[1]", "Claude"), ("[0]", "Codex"), ("[o]", "Rooms"),
                           ("[k]", "Capacity"), ("[9]", "Refresh"), ("[?]", "More"),
                           ("[q]", "Quit"), ("[t]", "Task")):
            for row in rows:
                if key in row:
                    assert label in row[row.index(key):], (columns, expanded, key, rows)
assert "Pi" not in " ".join(panel.keys_help(partial_installed))
# Expanded help keeps every shortcut group however narrow the pane is.
for columns in range(28, 41):
    expanded_rows = "\n".join(menu_rows(columns, 40, expanded=True))
    assert all(key in expanded_rows for key in ("[c]", "[7]", "[n]", "[8]")), (columns, expanded_rows)
partial_screen = render(empty_room, "claude", 57, 28, menu=True, navigation={"installed": partial_installed})
assert "Pi" not in partial_screen and "Press 1 (Claude)" in partial_screen and "0 opens Codex" in partial_screen
assert "2 (Pi)" not in partial_screen
for onboarding_width in (22, 57, 40):
    onboarding_screen = render(empty_room, "claude", onboarding_width, 28, menu=True,
                               navigation={"installed": {"claude", "pi"}})
    assert all(("1 (Claude)" in row or "Press" not in row) for row in onboarding_screen.splitlines()), onboarding_screen
    assert all(("2 (Pi)" in row or "or 2" not in row) for row in onboarding_screen.splitlines()), onboarding_screen
from room_view import render_graph
terminal_room = deepcopy(inbox_room)
terminal_room["attempts"]["active_recent"][0]["state"] = "done"
unknown_main_room = deepcopy(inbox_room)
unknown_main_room["attempts"]["active_recent"] = unknown_main_room["attempts"]["active_recent"][1:]
for columns, rows in ((50, 18), (80, 40), (127, 40)):
    for renderer in (lambda report, columns, rows, **kwargs: render(report, "claude", columns, rows, **kwargs),
                     render_graph, render_tree):
        for menu_nav in ({"installed": ["pi"]}, {"installed": ["pi"], "capacity_stale": True}):
            occupied_screen = renderer(inbox_room, columns, rows, menu=True, managed=True, navigation=dict(menu_nav))
            assert not any(key in occupied_screen for key in ("START", "[1]", "[2]", "[0]", "Press 1")), occupied_screen
            assert "[k]" in occupied_screen and "[q]" in occupied_screen, occupied_screen
        empty_screen = renderer(empty_room, columns, rows, menu=True, managed=True, navigation={"installed": ["pi"]})
        assert "[2] Pi" in empty_screen and "[1]" not in empty_screen and "[0]" not in empty_screen, empty_screen
        assert "[1] Codex" not in empty_screen and "[2] Claude" not in empty_screen, empty_screen
        occupied_nav = {"installed": ["pi"], "spawn_managed": True}
        board = renderer(inbox_room, columns, rows, navigation=occupied_nav, managed=True)
        assert occupied_nav["main_occupied"] and "[+ Pi" not in board, board
        for menu in (False, True):
            reused_nav = {"installed": ["pi"], "spawn_managed": True}
            renderer(inbox_room, columns, rows, menu=menu, navigation=reused_nav, managed=True)
            assert reused_nav["main_occupied"], reused_nav
            restored = renderer(terminal_room, columns, rows, menu=menu, navigation=reused_nav, managed=True)
            assert not reused_nav["main_occupied"], reused_nav
            assert ("[2] Pi" if menu else "[+ Pi") in restored, restored
            unknown = renderer(unknown_main_room, columns, rows, menu=menu, navigation=reused_nav, managed=True)
            assert reused_nav["main_occupied"] and "[2]" not in unknown and "[+ Pi" not in unknown, unknown
    for fallback_room in ({"id": "broken", "error": "room unreadable: ValueError: bad room"},
                          {"id": "no-participants", "participants": []}):
        fallback_report = dict(inbox_room, room=fallback_room)
        for fallback_nav in (None, {"installed": ["pi"]}):
            fallback = render(fallback_report, "claude", columns, rows, view="graph", menu=True,
                              managed=True, navigation=fallback_nav)
            assert not any(key in fallback for key in ("START", "[1]", "[2]", "[0]", "1 Codex", "2 Claude")), fallback
            assert "[k]" in fallback and "[q]" in fallback, fallback
            if fallback_nav is not None:
                assert fallback_nav["main_occupied"], fallback_nav
            assert len(fallback.splitlines()) <= rows - 1 and all(display_width(line) <= columns for line in fallback.splitlines()), fallback
    fallback_nav = {"installed": ["pi"], "main_occupied": True}
    fallback = render(empty_room, "claude", columns, rows, view="graph", menu=True,
                      managed=True, navigation=fallback_nav)
    assert not fallback_nav["main_occupied"] and "[2] Pi" in fallback, fallback
    assert not any(key in fallback for key in ("[1]", "[0]", "1 Codex", "2 Claude")), fallback
from room_view import footer_hints
for columns in (36, 50, 80, 127, 157):
    for managed in (False, True):
        for glyphs in (False, True):
            hint_nav = {"tab_strip_visible": True, "tab_body_visible": True, "tab": "detail",
                        "selected": ("chat", "flow-main")}
            base_footer = footer_hints(hint_nav, columns, managed, glyphs, None, True)[0]
            hint_nav["full_mail"] = ("mail", "flow-main")
            mail_footer = footer_hints(hint_nav, columns, managed, glyphs, None, True)[0]
            assert display_width(mail_footer) <= columns and (glyphs or mail_footer.isascii()), mail_footer
            assert "Tab Next tab" not in base_footer or "Tab Next tab" in mail_footer, (base_footer, mail_footer)
            assert managed or "f Open right" not in mail_footer, mail_footer
            if columns >= 127:
                assert "Tab Next tab" in mail_footer and ("f Open right" if managed else "f Full text") in mail_footer
                assert mail_footer.index("Tab Next tab") < mail_footer.index("f "), mail_footer
for columns in (36, 50, 80, 127):
    help_rows = footer_hints({"keys_help": True, "main_occupied": True}, columns, True, True, None, False)
    assert "? Hide" in "\n".join(help_rows) and "n New main" not in "\n".join(help_rows), help_rows
    assert len(help_rows) <= 4 and all(display_width(line) <= columns for line in help_rows), help_rows
    if columns in (50, 80, 127):
        for tabs in (False, True):
            for reader in (False, True):
                for has_call in (False, True):
                    hint_nav = {"tab_strip_visible": tabs, "tab": "messages" if tabs else None,
                                "message_has_call": has_call, "reader_open": reader,
                                "selected": ("result", "call")}
                    hints = footer_hints(hint_nav, columns, True, True, None, True)
                    foot = " ".join(hints)
                    assert all(display_width(line) <= columns for line in hints), hints
                    enter_hint = "Enter Expand" if tabs else "Enter Open right"
                    assert enter_hint in foot, (columns, hint_nav, hints)
                    if reader:
                        assert "Esc Close right" in foot, (columns, hint_nav, hints)
                    if columns >= 80 and tabs and has_call:
                        assert "f Open right" in foot, (columns, hint_nav, hints)
                    if columns >= 80 and not tabs:
                        assert "v Expand" in foot, (columns, hint_nav, hints)
assert "F9 Swap" in footer_hints({"selected": ("chat", "m1")}, 46, True, True, None, False)[0]
cancel_hint = footer_hints({"selected": ("result", "c1"),
                            "cancel_owner": {"participant": "m1", "attempt": "a1"}}, 100, True, True,
                           {"state": "working", "parent": "m1"}, True)
assert "c Cancel call" in " ".join(cancel_hint), cancel_hint
confirm_hint = footer_hints({"selected": ("result", "c1"),
                             "cancel_owner": {"participant": "m1", "attempt": "a1"},
                             "cancel_pending": {"call_id": "c1"}}, 100, True, True,
                            {"state": "working", "parent": "m1"}, True)
assert "Confirm in tmux prompt" in " ".join(confirm_hint), confirm_hint
expanded_message_nav = {"tab_strip_visible": True, "tab": "messages",
                        "messages_view": {"selected": "message-1", "open": "message-1"}}
assert "Enter Collapse" in " ".join(footer_hints(expanded_message_nav, 80, True, True, None, True))
assert "Sol 6.1 worker for Opus 5.5 · failed: Fix parser" in inbox, inbox
assert "waiting for approval: Run migration" in inbox and "patch awaits admission: Write patch" in inbox, inbox
assert "Opus 5.5 main · to all: Which branch?" in inbox and "\033" not in inbox, inbox
assert [h["action"] for h in nav["hits"]] == [("result", "w2"), ("result", "w1"), ("result", "w3"), ("chat", "m1")], nav
assert nav["items"][0] == ("result", "w2") and nav["selected"] == ("result", "w2")
choose(("down",), nav)
assert nav["selected"] == nav["items"][1]
# Two newer broadcasts to the same main remain separate rows and keyboard choices.
duplicate_room = deepcopy(inbox_room)
duplicate_room["room"]["participants"] = duplicate_room["room"]["participants"][:2]
duplicate_room["attempts"]["active_recent"] = duplicate_room["attempts"]["active_recent"][:2]
duplicate_room["room"]["messages"] = [dict(inbox_room["room"]["messages"][0], id="n%s" % number,
    text="Message %s" % number, ts="2026-10-07T00:0%s:00Z" % (number + 2)) for number in (1, 2)]
for columns, rows in ((100, 28), (24, 18)):
    duplicate_nav = {}
    render(duplicate_room, "codex", columns, rows, menu=True, navigation=duplicate_nav)
    assert duplicate_nav["items"] == [("chat", "m1"), ("chat", "m1"), ("result", "w1")]
    first, second = duplicate_nav["row_ids"][:2]
    assert first != second and duplicate_nav["selected_row"] == first
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected_row"] == second and duplicate_nav["selected"] == ("chat", "m1")
    assert choose(("enter",), duplicate_nav) == ("chat", "m1")
    assert render(duplicate_room, "codex", columns, rows, menu=True, navigation=duplicate_nav)
    assert duplicate_nav["selected_row"] == second
    second_hit = next(hit for hit in duplicate_nav["hits"] if hit["row_id"] == second)
    assert choose(("click", second_hit["x1"], second_hit["y"]), duplicate_nav) == ("chat", "m1")
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected"] == ("result", "w1")
    choose(("down",), duplicate_nav)
    assert duplicate_nav["selected_row"] == first
    choose(("scroll", 2, 1, 1), duplicate_nav)
    assert duplicate_nav["selected"] == ("result", "w1")
# The single-main board's control list has no folds: Space, Left and Right change nothing there.
nav["selected"] = ("chat", "m1")
for key in (" ", "left", "right"):
    assert choose((key,), nav) is None and nav["selected"] == ("chat", "m1") and "collapsed" not in nav, key
small_nav = {}
small_inbox = render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav)
assert choose(("scroll", 1, 5, 5), small_nav) is None
assert small_nav["selected"] == small_nav["items"][1]
choose(("pagedown",), small_nav)
assert small_nav["selected"] == small_nav["items"][-1]
scrolled_inbox = render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav)
assert scrolled_inbox != small_inbox and small_nav["selected"] in [hit["action"] for hit in small_nav["hits"]]
assert render(inbox_room, "codex", 100, 8, menu=True, navigation=small_nav) == scrolled_inbox
choose(("pagedown",), small_nav)
choose(("scroll", 3, 5, 5), small_nav)
assert small_nav["selected"] == small_nav["items"][-1]
choose(("pageup",), small_nav)
assert small_nav["selected"] == small_nav["items"][1]
choose(("scroll", -20, 5, 5), small_nav)
choose(("pageup",), small_nav)
assert small_nav["selected"] == small_nav["items"][0]
render(inbox_room, "codex", 100, 28, menu=True, navigation=small_nav)
assert small_nav["selected"] == small_nav["items"][0]
empty_nav = {}
render(empty_room, "codex", 100, 8, menu=True, navigation=empty_nav)
for event in (("scroll", 3, 5, 5), ("pageup",), ("pagedown",)):
    assert choose(event, empty_nav) is None and empty_nav["selected"] is None and empty_nav["offset"] == 0
for width, height in ((100, 28), (42, 24), (24, 18), (12, 10)):
    for glyphs in (True, False):
        drawn = render(inbox_room, "codex", width, height, unicode=glyphs, menu=True, navigation={})
        assert len(drawn.splitlines()) <= height - 1 and all(display_width(line) <= width for line in drawn.splitlines()), drawn
        closed_cards(drawn, width, glyphs)
assert "key + Enter" in render(inbox_room, "codex", 100, 28, menu=True, navigation={"line_input": True})
assert panel.menu_input().decode(b"1\033[B2q ") == [("1",), ("down",), ("2",), ("q",), (" ",)]
# Starting/resuming while occupied or unreadable refuses before any prompt or room mutation.
occupied_control = deepcopy(inbox_room)
occupied_control["attempts"]["active_recent"][0]["attempt_id"] = "att_control_main"
for status, control_report in ((0, occupied_control), (1, {"collection": {"ok": False}})):
    choices = iter(["1", "2", "0", "3", "t", "q"])
    previous_values = []
    def occupied_choice(*args):
        args[-1].update(status=status, state=control_report)
        previous_values.append(args[2])
        return next(choices)
    with patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "read_choice", side_effect=occupied_choice), \
            patch.object(panel, "events", return_value=json.dumps([{"attempt_id": "a1", "tool": "panel-main", "state": "working"}])) as history, \
            patch.object(panel, "read_field") as prompts, patch.object(panel, "panel_room") as room_change, \
            patch.object(panel, "open_native") as launches:
        assert panel.interactive(project) == 0
        prompts.assert_not_called()
        room_change.assert_not_called()
        launches.assert_not_called()
    if status == 0:
        assert previous_values[-1] == "error: this panel already has a live main or unverified owner; if crashed: oms panel --release-main att_control_main"
        history.assert_not_called()
# A recoverable older attempt does not replace the occupying main in the refusal.
old_main = {"attempt_id": "att_old_main", "tool": "panel-main", "state": "working", "refs": {"panel_role": "main"}}
old_hint = "; if that main crashed, release it with oms panel --release-main att_old_main (see k)"
unknown_hint = "; see capacity screen k for recovery evidence; candidate is unverifiable; release requires confirmed termination"
for live_row in ({"attempt_id": "att_occupying", "tool": "panel-main", "state": "working", "panel": {"role": "main"}},
                 {"attempt_id": "att_occupying", "tool": "panel-main", "state": "working", "refs": {"panel_role": "main"}}):
    with patch("panel_release.refusal_hint", side_effect=lambda rows: old_hint if rows else unknown_hint), \
            patch("panel_release.heartbeat_fresh", side_effect=lambda row: row["attempt_id"] == "att_occupying"):
        refusal = panel.live_main_refusal(project, [old_main, live_row])
        assert refusal.endswith("oms panel --release-main att_occupying"), refusal
        assert "att_old_main" not in refusal, refusal
        ambiguous = panel.live_main_refusal(project, [old_main, live_row, dict(live_row, attempt_id="att_other", panel={"role": "main"})])
        assert "capacity screen k" in ambiguous and "--release-main" not in ambiguous, ambiguous
# A maximum-length release command stays intact and copyable in the control window.
maximum_control_attempt = "a" + "._:-09AZaz" * 15 + "._:-09AZa"
maximum_control_command = "oms panel --release-main " + maximum_control_attempt
maximum_control = deepcopy(occupied_control)
maximum_control["attempts"]["active_recent"][0]["attempt_id"] = maximum_control_attempt
previous_values = []
choices = iter(["1", "q"])
def maximum_occupied_choice(*args):
    args[-1].update(status=0, state=maximum_control)
    previous_values.append(args[2])
    return next(choices)
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "read_choice", side_effect=maximum_occupied_choice), \
        patch.object(panel, "events", return_value=json.dumps([{"attempt_id": "a1",
                                                               "tool": "panel-main", "state": "working"}])) as history, \
        patch.object(panel, "read_field") as prompts, patch.object(panel, "panel_room") as room_change, \
        patch.object(panel, "open_native") as launches:
    assert panel.interactive(project) == 0
    prompts.assert_not_called()
    room_change.assert_not_called()
    launches.assert_not_called()
    history.assert_not_called()
assert previous_values[-1].splitlines()[1] == maximum_control_command, previous_values
for columns in (50, 100, 200):
    for rows in (18, 28):
        for glyphs in (True, False):
            for colored in (True, False):
                picture = render(maximum_control, "codex", columns, rows, unicode=glyphs,
                                 color=colored, menu=True, previous=previous_values[-1], navigation={})
                rendered_rows = re.sub(r"\x1b\[[0-9;]*m", "", picture).splitlines()
                command_at = next(i for i, line in enumerate(rendered_rows)
                                  if line.startswith("oms panel --release-main "))
                command_rows = rendered_rows[command_at:command_at + (len(maximum_control_command) + columns - 1) // columns]
                assert "".join(command_rows) == maximum_control_command, picture
                assert all(display_width(line) <= columns for line in rendered_rows), picture
                assert "".join(rendered_rows[next(i for i, line in enumerate(rendered_rows)
                                                  if line.startswith("Last: ")):command_at]) == (
                    "Last: " + previous_values[-1].splitlines()[0]), picture
short_refusal = "error: this panel already has a live main or unverified owner; if crashed: oms panel --release-main att_control_main"
short_picture = render(occupied_control, "codex", 200, 28, menu=True,
                       previous=short_refusal, navigation={})
assert short_picture.splitlines().count("Last: " + short_refusal) == 1, short_picture
for columns in (50, 80, 127):
    short_picture = render(occupied_control, "claude", columns, 18, menu=True, previous=short_refusal)
    short_rows = short_picture.splitlines()
    start = next(i for i, line in enumerate(short_rows) if line.startswith("Last: "))
    assert "".join(short_rows[start:start + (len("Last: " + short_refusal) + columns - 1) // columns]) == "Last: " + short_refusal, short_picture
# A healthy read retained after a failed refresh is display evidence only.
class StaleControlInput:
    fd = None
    def __enter__(self): return self
    def __exit__(self, *args): pass
for columns, rows in ((50, 18), (80, 40), (127, 40)):
    keys = iter(["9", "1", "2", "0", "3", "t", "q"])
    def stale_key(prompt, **kwargs):
        assert prompt == "oms> ", "stale evidence reached an action prompt: " + prompt
        return next(keys)
    with patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "menu_input", return_value=StaleControlInput()), \
            patch.object(panel, "raw_keys", return_value=False), \
            patch.object(panel, "snapshot", side_effect=[(empty_room, 0)] + [({"collection": {"ok": False}}, 1)] * 6), \
            patch.object(panel, "events", return_value="[]"), \
            patch.object(panel, "read_field", side_effect=stale_key), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((columns, rows))), \
            patch.object(panel, "panel_room") as room_change, patch.object(panel, "open_native") as launches, \
            patch.object(panel.sys, "stdout", io.StringIO()) as stale_control:
        assert panel.interactive(project) == 0
        room_change.assert_not_called()
        launches.assert_not_called()
        output = stale_control.getvalue()
        failed_frames = output[output.index("State refresh failed;"):]
        assert "START" not in failed_frames and "Press 1" not in failed_frames and "[1]" not in failed_frames, failed_frames
        assert "live main" in failed_frames, failed_frames
# Failed room queries keep healthy display evidence inert; a successful poll restores starts.
for poll_failure in (subprocess.CompletedProcess(["tmux"], 1, "", "server unavailable"),
                     subprocess.TimeoutExpired("tmux", 3),
                     subprocess.CompletedProcess(["tmux"], 0, "OMS_ROOM_ID=../invalid\n", "")):
    poll_keys = iter(["9", "1", "t", "2", "0", "3", "1"])
    poll_frames = []
    poll_results = iter([poll_failure, subprocess.CompletedProcess(["tmux"], 0, "OMS_ROOM_ID=selected-room\n", "")])
    def poll_tmux(command, **kwargs):
        if "show-environment" in command:
            result = next(poll_results)
            if isinstance(result, Exception):
                raise result
            return result
        return subprocess.CompletedProcess(command, 0, "", "")
    def poll_choice(*args):
        nav, cache = args[-2:]
        assert os.environ["OMS_ROOM_ID"] == "selected-room"
        if poll_frames:
            assert nav["selected"] == ("chat", "selected-main"), nav
        nav["selected"] = ("chat", "selected-main")
        cache.update(state=empty_room, status=0)
        poll_frames.append((args[2], dict(nav)))
        if len(poll_frames) <= 7:
            prompts.assert_not_called()
            room_change.assert_not_called()
            launches.assert_not_called()
        choice = next(poll_keys, None)
        if choice is None:
            # Managed q only detaches; EOF ends the control loop.
            raise EOFError
        return choice
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-poll-fixture", "OMS_ROOM_ID": "selected-room"}, clear=True), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel.time, "monotonic", side_effect=[0, 0, 1, 2, 3, 4, 4.5, 5, 5, 6]), \
            patch.object(panel.subprocess, "run", side_effect=poll_tmux) as polls, \
            patch.object(panel, "room_open", return_value=True) as open_room, \
            patch.object(panel, "read_choice", side_effect=poll_choice), \
            patch.object(panel, "events", return_value="[]"), patch.object(panel, "read_field") as prompts, \
            patch.object(panel, "panel_room", return_value="selected-room") as room_change, \
            patch.object(panel, "open_native", return_value=0) as launches:
        assert panel.interactive(project) == 0
        queries = [call for call in polls.call_args_list if "show-environment" in call.args[0]]
        assert len(queries) == 2 and all("OMS_ROOM_ID" not in call.args[0] for call in queries), queries
        assert "Room selection unknown" in poll_frames[0][0], poll_frames
        for previous, nav in poll_frames[:6]:
            assert nav["room_poll_unknown"] and "Room selection unknown" in previous, poll_frames
            drawn = render(empty_room, "codex", 100, 28, menu=True, navigation=nav)
            assert "START" not in drawn and "Press 1" not in drawn and "[1]" not in drawn, drawn
        assert all(not nav.get("room_poll_unknown") for previous, nav in poll_frames[6:]), poll_frames
        prompts.assert_not_called()
        room_change.assert_called_once_with(project)
        launches.assert_called_once_with("claude", project, None, view="auto", attention_only=False)
        open_room.assert_called_once_with(project, "selected-room")
# An unset session room clears adoption and blocks launch; a later valid poll adopts again.
for unset_output in ("", "-OMS_ROOM_ID\n", "OMS_ROOM_ID=\n"):
    unset_frames = []
    unset_results = iter([unset_output, "OMS_ROOM_ID=recovered-room\r\n"])
    def unset_tmux(command, **kwargs):
        return subprocess.CompletedProcess(command, 0, next(unset_results) if "show-environment" in command else "", "")
    def unset_choice(*args):
        nav, cache = args[-2:]
        unset_frames.append(os.environ.get("OMS_ROOM_ID"))
        assert bool(nav.get("room_poll_unknown")) == (len(unset_frames) == 1), nav
        cache.update(state=empty_room, status=0)
        if len(unset_frames) == 1:
            assert unset_frames[-1] is None, unset_frames
            return "1"
        assert unset_frames[-1] == "recovered-room", unset_frames
        raise EOFError
    def fresh_room(repo):
        assert "OMS_ROOM_ID" not in os.environ, dict(os.environ)
        return "fresh-room"
    with patch.dict(os.environ, {"OMS_PANEL_SESSION": "oms-poll-fixture", "OMS_ROOM_ID": "selected-room"}, clear=True), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel.time, "monotonic", side_effect=[0, 0, 5, 5]), \
            patch.object(panel.subprocess, "run", side_effect=unset_tmux), \
            patch.object(panel, "room_open", return_value=True) as open_room, \
            patch.object(panel, "read_choice", side_effect=unset_choice), \
            patch.object(panel, "panel_room", side_effect=fresh_room) as room_change, \
            patch.object(panel, "open_native", return_value=0) as launches:
        assert panel.interactive(project) == 0
        room_change.assert_not_called()
        launches.assert_not_called()
        open_room.assert_called_once_with(project, "recovered-room")
# Existing callers still receive None on failure; a successful unset is not a query failure.
for code, output in ((1, ""), (0, "-OMS_ROOM_ID\n")):
    with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(["tmux"], code, output, "")):
        assert panel.recorded_room("oms-poll-fixture") is None
        if code == 0:
            assert panel.recorded_room("oms-poll-fixture", checked=True) is None
for output, expected in (("OTHER_ROOM_ID=unrelated-room\n", None),
                         ("OTHER_ROOM_ID=unrelated-room\nOMS_ROOM_ID=selected-room\n", "selected-room")):
    with patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess(["tmux"], 0, output, "")) as query:
        assert panel.recorded_room("oms-poll-fixture", checked=True) == expected
        assert "OMS_ROOM_ID" not in query.call_args.args[0], query.call_args
for answer, expected in (("", None), ("1", "selected-room"), ("n", "new-room")):
    with patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "read_choice", side_effect=["o", "q"]), \
            patch.object(panel.room, "discover", return_value=([{"id": "selected-room", "title": "Shared"}], False)), \
            patch.object(panel, "read_field", side_effect=[answer, "New title"]), \
            patch.object(panel, "ensure_room", return_value="selected-room") as chosen_room, \
            patch.object(panel.room, "create", return_value="new-room") as created_room, \
            patch.dict(os.environ, {}, clear=True), patch.object(panel.sys, "stdout", io.StringIO()) as rooms_text:
        assert panel.interactive(project) == 0
        assert "1 selected-room / Shared" in rooms_text.getvalue() and "{\"" not in rooms_text.getvalue()
        assert os.environ.get("OMS_ROOM_ID") == expected
        assert chosen_room.called == (answer == "1") and created_room.called == (answer == "n")
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "read_choice", side_effect=["o", "q"]), \
        patch.object(panel.room, "discover", side_effect=SystemExit(2)), \
        patch.object(panel, "read_field") as prompts:
    assert panel.interactive(project) == 0
    prompts.assert_not_called()

# Enter on a result row in the control window opens its recorded result; no watch loop is there to read it.
class OpenInput:
    fd, batches = 0, [[("enter",)], [("q",)]]
    def __enter__(self): return self
    def __exit__(self, *exc): return False
    def wait(self, seconds): return self.batches.pop(0)
open_nav = {"items": [("result", "w1")], "selected": ("result", "w1")}
def request_open(repo, action, state, navigation, width):
    navigation["opening"] = action
shown_detail = []
with patch.object(panel, "menu_input", return_value=OpenInput()), \
        patch.object(panel, "show", side_effect=lambda *a, **k: shown_detail.append((open_nav.get("detail") or {}).get("title"))), \
        patch.object(panel, "navigate", side_effect=request_open), \
        patch.object(panel, "read_evidence", side_effect=lambda repo, target: (target, {"rows": [{"calls": [1]}]}, None)):
    panel.read_choice(Path("."), "codex", "ready", "auto", False, open_nav, {})
assert shown_detail[-1] == "RECORDED RESULTS" and "opening" not in open_nav, (shown_detail, open_nav)
# A full reader ignores clicks and Enter without repainting; a wheel step still redraws.
class ReaderInput:
    fd, batches = 0, [[("click", 3, 3), ("enter",)], [("scroll", 3)], [("q",)]]
    def __enter__(self): return self
    def __exit__(self, *exc): return False
    def wait(self, seconds): return self.batches.pop(0)
reader_nav = {"detail": {"target": ("result", "builder")}, "offset": 2}
with patch.object(panel, "menu_input", return_value=ReaderInput()), patch.object(panel, "show") as painted, \
        patch("panel_input.choose", wraps=choose) as chosen:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, reader_nav, {}) == "9"
assert painted.call_count == 2 and reader_nav["offset"] == 0 and not any(
    c.args[0][0] in ("click", "enter") for c in chosen.call_args_list), (painted.call_count, reader_nav)
for key, expected in (("k", "k"), ("u", "9")):
    reader_input = ReaderInput()
    reader_input.batches = [[(key,)]]
    reader_nav = {"detail": {"target": ("result", "builder")}, "offset": 2}
    with patch.object(panel, "menu_input", return_value=reader_input), patch.object(panel, "show"):
        assert panel.read_choice(Path("."), "codex", "ready", "auto", False, reader_nav, {}) == expected
    assert "detail" not in reader_nav and reader_nav["offset"] == 0

# A rendered graph preview: a click on plain Detail text repaints nothing; a card click and the wheel still do.
graph_nav = {"tab": "detail", "preview": {"target": ("chat", "flow-main"), "report": {}}}
render(mailed, "codex", 100, 30, view="graph", main_attempt="flow-attempt", navigation=graph_nav)
text_hit = next(h for h in graph_nav["hits"] if h.get("passive"))
card_hit = next(h for h in graph_nav["hits"] if h["action"] == ("chat", "flow-main") and not h.get("passive") and h.get("preview"))
for event in (("click", card_hit["x1"], card_hit["y"]), ("click", card_hit["x1"], card_hit["y"]), ("enter",)):
    assert choose(event, graph_nav) is None and graph_nav["preview"]["target"] == ("chat", "flow-main")
assert graph_nav["preview"]["report"] == {}, graph_nav
box = next(b for b in graph_nav["bands"] if b["name"] == "detail")
class GraphInput(ReaderInput):
    batches = [[("click", text_hit["x1"], text_hit["y"])], [("click", card_hit["x1"], card_hit["y"])],
               [("scroll", 3, 5, box["y1"] + 1)], [("q",)]]
with patch.object(panel, "menu_input", return_value=GraphInput()), patch.object(panel, "show") as painted, \
        patch.object(panel, "navigate") as moved:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, graph_nav, {}) == "q"
# The first frame, the card click and the wheel paint; the plain-text click does not.
assert painted.call_count == 3 and graph_nav["preview"].get("report") == {} and moved.call_count == 0, (
    painted.call_count, moved.call_args_list)
# The control pane routes tree and graph dock events before returning a control choice.
class DockInput(ReaderInput):
    def __init__(self, batches): self.batches = batches
dock_state = {"collection": {"ok": True},
              "room": {"id": "room-1", "message_count": 3, "pending_count": 1, "received_count": 2,
                       "participants": [{"participant": "main-1", "role": "main", "joined": True,
                                         "provider": "codex", "attempt": "main-attempt"}]},
              "attempts": {"recent": [{"attempt_id": "debate-attempt", "tool": "panel-council",
                                       "task_id": "task-1", "state": "done", "parent_attempt_id": "main-attempt",
                                       "panel": {"room_id": "room-1", "room_participant": "main-1"}}]}}
def dock_read(navigation, state=dock_state, batches=None):
    # Use the real show/frame_view path, including an empty cache on each automatic refresh.
    clock = [0]
    class RefreshInput(DockInput):
        def wait(self, seconds):
            clock[0] += seconds
            return []
    inputs = RefreshInput([]) if batches is None else DockInput(batches)
    width = 120 if navigation["surface"] == "graph" else 70  # the graph view's tree fallback
    with patch.object(panel, "menu_input", return_value=inputs), \
            patch.object(panel, "snapshot", return_value=(state, 0)) as collected, \
            patch("panel_metrics.collect", return_value={}) as metrics, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "provider_availability", return_value={}), \
            patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((width, 40))), \
            patch.object(panel.sys, "stdout", io.StringIO()) as output:
        choice = panel.read_choice(Path("."), "codex", "ready", "graph", False,
                                   navigation, {})
    collected.assert_called_once_with(Path("."))
    metrics.assert_called_once_with(state, None, repo=Path("."))
    return choice, output.getvalue()

# Attention Enter returns menu f with the exact call and task, rather than opening the retained reader.
attention_control = deepcopy(dock_state)
attention_control["room"]["participants"].append({"participant": "pending-call", "role": "worker", "joined": True,
                                                   "parent": "main-1", "provider": "codex", "model": "gpt-6.1-sol"})
attention_control["attempts"].setdefault("active_recent", []).append({"attempt_id": "pending-attempt", "task_id": "pending-task",
    "state": "review", "panel": {"room_id": "room-1", "room_participant": "pending-call", "role": "worker",
                                  "label": "Pending task"}})
for surface in ("graph", "tree"):
    finalize_nav = {"surface": surface, "tab": "needs"}
    choice, _ = dock_read(finalize_nav, state=attention_control, batches=[[('home',)], [('enter',)]])
    assert choice == "f" and finalize_nav["finalize_call"] == {"task_id": "pending-task", "target": ("result", "pending-call")}, finalize_nav
# The watch board restores normal input for the same finalize prompts, then resumes board input.
class FinalizeWatchInput(DockInput):
    fd, active = None, False
    transitions = []
    def __enter__(self):
        self.active = True
        self.transitions.append('enter')
        return self
    def __exit__(self, *exc):
        self.active = False
        self.transitions.append('exit')
        return False
    def discard_clicks(self): pass
watch_inputs = FinalizeWatchInput([[('home',)], [('enter',), ('q',)], [('q',)]])
watch_finalized = []
def watch_finalize(repo, provider, navigation, state):
    assert not watch_inputs.active, 'owner prompts received raw board input'
    watch_finalized.append(navigation.pop('finalize_call'))
    assert state is attention_control and provider == 'codex'
    return 'result completed / app not_requested'
with patch.dict(os.environ, {'OMS_PANEL_RESUME': json.dumps({'room_id': 'room-1', 'tab': 'needs'})}), \
        patch('panel_input.TerminalInput', return_value=watch_inputs), \
        patch.object(panel, 'read_panel', return_value=(attention_control, 0, 'codex', None, 'graph', False, 'room-1')), \
        patch.object(panel, 'finalize_selection', side_effect=watch_finalize), \
        patch.object(panel, 'terminal_style', return_value=(True, False, True)), \
        patch.object(panel, 'managed_session', return_value=False), \
        patch.object(panel, 'provider_availability', return_value={}), \
        patch.object(panel.shutil, 'get_terminal_size', return_value=os.terminal_size((120, 40))), \
        patch.object(panel.sys, 'stdout', io.StringIO()):
    assert panel.watch(Path('.'), 'codex', no_animation=True, view='graph') == 0
assert watch_finalized == [{'task_id': 'pending-task', 'target': ('result', 'pending-call')}], watch_finalized
assert watch_inputs.transitions == ['enter', 'exit', 'enter', 'exit'], watch_inputs.transitions
assert not watch_inputs.batches, 'keys queued before the prompt survived its handoff'
for surface in ("graph", "tree"):
    dock_nav = {"surface": surface, "tab": "detail", "hits": [], "tab_strip_visible": True}
    choice, text = dock_read(dock_nav, batches=[[('tab',)], [('q',)]])
    assert choice == "q" and "No repository plan recorded" in text, text
    assert dock_nav["tab"] == "plan", dock_nav
# Main mail control input consumes f and uses the same target as the board, with a local reader fallback.
dock_mail = deepcopy(dock_state)
dock_mail["room"].update(closed=False, messages=[{"sender": "advisor", "targets": ["main-1"],
                                                "text": mail_body, "pending_for": ["main-1"]}])
for surface in ("graph", "tree"):
    mail_control_nav = {"surface": surface, "tab": "detail", "preview": {"target": ("chat", "main-1")}}
    with patch.object(panel.room, "status", return_value=dock_mail["room"]), \
            patch.object(panel.panel_messages, "read", return_value=dock_mail["room"]["messages"]) as mail_log, \
            patch.object(panel.room, "acknowledge", side_effect=AssertionError("control acknowledged mail")):
        choice, text = dock_read(mail_control_nav, state=dock_mail, batches=[[('f',)], [('q',)]])
    # The reader page comes from the whole bounded log, not the status window.
    assert mail_log.called, "mail reader skipped the full log"
    assert choice == "9" and "LAST MAIL LINE" in text and "```bash" in text and "|------|-------|" in text, text
    assert mail_control_nav.get("offset") == 0 and "detail" not in mail_control_nav
    empty_control_nav = {"surface": surface, "tab": "detail", "preview": {"target": ("chat", "main-1")}}
    with patch.object(panel.room, "status", side_effect=AssertionError("empty mail was opened")):
        choice, text = dock_read(empty_control_nav, batches=[[('f',)], [('q',)]])
    assert choice == "q" and "f Open right" not in text and not empty_control_nav.get("detail"), text
mail_control_nav = {"surface": "graph", "tab": "detail", "preview": {"target": ("chat", "main-1")}}
with patch.object(panel, "menu_input", return_value=DockInput([[('f',)], [('escape',)], [('q',)]])), \
        patch.object(panel, "_show_state", return_value=(dock_mail, 0)), \
        patch.object(panel, "show", side_effect=lambda *a, **k: render(dock_mail, "codex", 120, 44, view="graph",
                     managed=True, unicode=False, navigation=mail_control_nav)), \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel.panel_host, "herdr_active", return_value=False), \
        patch.dict(os.environ, {"TMUX_PANE": "%1"}, clear=True), \
        patch.object(panel.panel_reader, "open_reader", return_value="%9") as opened, \
        patch.object(panel.panel_reader, "close_reader", return_value=True) as closed:
    assert panel.read_choice(project, "codex", "ready", "graph", False, mail_control_nav, {}) == "q"
closed.assert_called_once_with("%1")
assert not mail_control_nav.get("reader_open") and shlex.split(opened.call_args.args[1])[-4:] == [
    "--mail", "main-1", "--room", "room-1"], opened.call_args
# End main mail control assertions.
# Debate evidence is loaded synchronously on entering the tab and retained across repaint.
for surface in ("graph", "tree"):
    debate_nav = {"surface": surface, "tab": "detail", "tab_strip_visible": True,
                  "debate_view": {"shown": ("task-1", "main-1")},
                  "box": {"y1": 1, "y2": 10}}
    answer = {"rows": [{"calls": [{"kind": "ask", "selected_model": "gpt-6.1-sol", "answer": "seat answer"}]}]}
    with patch.object(panel.panel_debate, "read", return_value=answer) as debate_read:
        choice, text = dock_read(debate_nav, batches=[[('tab',)], [('tab',)], [('scroll', 1)], [('q',)]])
        assert choice == "q" and "seat answer" in text, text
    debate_read.assert_called_once_with(Path("."), ("debate", ("task-1", "main-1")), panel.call_results)
    assert debate_nav["debate_view"]["report"] is answer and debate_nav["tab"] == "debate", debate_nav
# Reader failures render, survive repaint without repeated reads, and clear on successful retry.
for surface in ("graph", "tree"):
    debate_nav = {"surface": surface, "tab": "debate", "tab_strip_visible": True,
                  "debate_view": {"shown": ("task-1", "main-1")}}
    with patch.object(panel.panel_debate, "read", side_effect=OSError("debate missing")) as debate_read:
        choice, text = dock_read(debate_nav, batches=[[('scroll', 1)], [('q',)]])
    assert choice == "q" and "! debate missing" in text and "No seat answers recorded yet" not in text, text
    debate_read.assert_called_once()
    assert debate_nav["debate_view"]["report"] is None, debate_nav
    debate_nav["debate_view"]["refresh"] = True
    with patch.object(panel.panel_debate, "read", return_value=answer) as debate_read:
        choice, text = dock_read(debate_nav)
    assert choice == "9" and "seat answer" in text and "debate missing" not in text, text
    debate_read.assert_called_once()
    assert debate_nav["debate_view"]["error"] is None, debate_nav

    messages_nav = {"surface": surface, "tab": "messages", "tab_strip_visible": True}
    messages = [{"id": "m1", "text": "first message"}]
    new_message = {"id": "m2", "text": "new message"}
    newer_state = dict(dock_state, room=dict(dock_state["room"], message_count=4))
    with patch.object(panel.panel_messages, "read", side_effect=[messages, messages + [new_message]]) as messages_read:
        choice, text = dock_read(messages_nav)
        assert choice == "9" and "first message" in text and "new message" not in text, text
        choice, text = dock_read(messages_nav, newer_state)
        assert choice == "9" and "new message" in text, text
    assert messages_read.call_count == 2, messages_read.call_args_list
    assert messages_nav["messages_view"]["rev"] == panel.panel_messages.signature(newer_state), messages_nav

    messages_nav = {"surface": surface, "tab": "messages", "tab_strip_visible": True}
    with patch.object(panel.panel_messages, "read", side_effect=OSError("messages missing")) as messages_read:
        choice, text = dock_read(messages_nav, batches=[[('scroll', 1)], [('scroll', 1)], [('q',)]])
        assert choice == "q" and "! messages missing" in text, text
        choice, text = dock_read(messages_nav)
        assert choice == "9" and "! messages missing" in text, text
    messages_read.assert_called_once_with(Path("."), "room-1")
    with patch.object(panel.panel_messages, "read", return_value=messages + [new_message]) as messages_read:
        choice, text = dock_read(messages_nav, newer_state)
    assert choice == "9" and "new message" in text and "messages missing" not in text, text
    messages_read.assert_called_once_with(Path("."), "room-1")
    assert messages_nav["messages_view"]["error"] is None, messages_nav
# A Messages-tab f request opens the recorded target before returning the queued control key.
message_nav = {"surface": "graph", "tab": "messages", "box": {"y1": 1, "y2": 10},
               "message_ids": ["m1"], "message_calls": {"m1": ("result", "w1")},
               "messages_view": {"selected": "m1"}}
def request_message_open(repo, target, state, navigation, width):
    navigation["opening"] = target
message_input = DockInput([[('f',)], [('k',)]])
with patch.object(panel, "menu_input", return_value=message_input), patch.object(panel, "show"), \
        patch.object(panel, "navigate", side_effect=request_message_open) as moved, \
        patch.object(panel, "read_evidence", return_value=(('result', 'w1'), {"rows": []}, None)) as evidence, \
        patch.object(panel, "finish_opening", wraps=panel.finish_opening) as opened:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, message_nav, {}) == "k"
moved.assert_called_once()
evidence.assert_called_once_with(Path("."), ("result", "w1"))
opened.assert_called_once()
assert "open_call" not in message_nav, message_nav
# Expanded interactive help uses the installed harness list from navigation.
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "panel_mains", return_value=["claude"]), \
        patch.object(panel, "main_owner", return_value="claude"), \
        patch.object(panel, "main_installed", side_effect=lambda name: name in {"claude", "codex"}), \
        patch.object(panel, "read_choice", side_effect=["?", "q"]), \
        patch.object(panel, "read_field", return_value="") as prompt, \
        patch.object(panel, "show"), \
        patch.object(panel.sys, "stdout", io.StringIO()) as help_text:
    assert panel.interactive(Path(".")) == 0
assert "Pi" not in help_text.getvalue(), help_text.getvalue()
with patch.object(panel, "menu_input", return_value=DockInput([[('x',)]])), \
        patch.object(panel, "show"):
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, dock_nav, {}) == "x"
# Empty and blank Detail body rows have no hit, but still must not trigger a redraw.
empty_graph = {"room": {"id": "empty-graph", "participants": []}}
empty_graph_nav = {"tab": "detail", "dismissed": True}
empty_graph_frame = render_graph(empty_graph, 100, 30, navigation=empty_graph_nav)
hint_graph = deepcopy(flow)
hint_graph["room"]["participants"] = [p for p in hint_graph["room"]["participants"] if p.get("role") == "main"]
hint_graph["attempts"]["active_recent"] = []
hint_graph["attempts"]["recent"] = []
hint_graph_nav = {}
hint_graph_80x39 = render_graph(hint_graph, 80, 39, unicode=False, navigation=hint_graph_nav)
assert "Workers 0/0 / Judges 0/0" in hint_graph_80x39 and "a Ask advisor" in hint_graph_80x39, hint_graph_80x39
assert not any(target[0] == "result" for target in hint_graph_nav["items"]), hint_graph_nav
hint_y = next(i for i, line in enumerate(empty_graph_frame.splitlines(), 1)
              if "No call selected" in line)
blank_y = next(i for i, line in enumerate(empty_graph_frame.splitlines(), 1)
               if empty_graph_nav["box"]["y1"] < i < empty_graph_nav["box"]["y2"]
               and line.startswith("│") and line.endswith("│") and not line.strip("│ "))
assert passive_click(("click", 50, hint_y), empty_graph_nav)
assert passive_click(("click", 50, blank_y), empty_graph_nav)
tab_hit = next(h for h in empty_graph_nav["hits"] if h["action"] == ("tab", "plan"))
assert not passive_click(("click", tab_hit["x1"], tab_hit["y"]), empty_graph_nav)
assert choose(("click", tab_hit["x1"], tab_hit["y"]), dict(empty_graph_nav)) == ("tab", "plan")
for default_tab in (None, "bogus"):
    assert passive_click(("click", 50, hint_y), dict(empty_graph_nav, tab=default_tab)), default_tab
for bad_nav, event in (
        (dict(empty_graph_nav, tab="plan"), ("click", 50, hint_y)),
        (dict(empty_graph_nav, surface="tree"), ("click", 50, hint_y)),
        (dict(empty_graph_nav, box={"y1": 2, "y2": 2}), ("click", 50, hint_y)),
        (empty_graph_nav, ("click", 1, hint_y)),
        (empty_graph_nav, ("click", 100, hint_y)),
        (empty_graph_nav, ("click", 50, empty_graph_nav["box"]["y1"]))):
    assert not passive_click(event, bad_nav), (event, bad_nav)
class EmptyGraphInput(ReaderInput):
    batches = [[("click", 50, hint_y)], [("click", 50, blank_y)], [("q",)]]
def draw_empty_graph(*args, **kwargs):
    render_graph(empty_graph, 100, 30, navigation=empty_graph_nav)
with patch.object(panel, "menu_input", return_value=EmptyGraphInput()), \
        patch.object(panel, "show", side_effect=draw_empty_graph) as painted, \
        patch.object(panel, "navigate") as moved:
    assert panel.read_choice(Path("."), "codex", "ready", "auto", False, empty_graph_nav, {}) == "q"
assert painted.call_count == 1 and moved.call_count == 0, (painted.call_count, moved.call_count)
assert TerminalInput().decode(b"v\033[A") == [("v",), ("up",)]
# Keys the board does not bind stay distinct from Esc; Home/End work in every encoding tmux sends.
for sequence, expected in ((b"\033[1~", "home"), (b"\033[4~", "end"), (b"\033[7~", "home"), (b"\033[8~", "end"),
                           (b"\033OH", "home"), (b"\033OF", "end")):
    assert TerminalInput().decode(sequence) == [(expected,)], sequence
for sequence in (b"\033[Z", b"\033[3~", b"\033[15~", b"\033[1;5C", b"\033OP"):
    ignored = TerminalInput()
    assert ignored.decode(sequence) == [("unbound",)] and ignored.pending == b"", sequence
assert TerminalInput().decode(b"\033[3~q") == [("unbound",), ("q",)]
split_paste = TerminalInput()
split_paste_events = split_paste.decode(b"\033[200")
split_paste_events += split_paste.decode(b"~" + b"x" * 1022 + b"c")
split_paste_events += split_paste.decode(b"y")
assert split_paste.pasting and split_paste_events == [("paste",)], \
    (split_paste_events, split_paste.pasting)
for partial, expected in ((b"\033", [("escape",)]), (b"\033[1;5", []), (b"\033O", [])):
    timed = TerminalInput()
    timed.fd = 0
    assert timed.decode(partial) == [] and timed.escape_since is not None
    timed.escape_since -= 1
    with patch("panel_input.select.select", return_value=([], [], [])):
        assert timed.wait(0) == expected and timed.pending == b"" and timed.escape_since is None, partial
assert TerminalInput().decode(b"?") == [("?",)]
# A transient snapshot failure keeps the board's folds, selection and view; only another room resets them.
held = {"room_id": "flow-room", "selected": ("chat", "m1"), "collapsed": {"m1"}, "offset": 4, "view": "graph", "attention": True}
transient = {"repo": {"name": "x"}, "collection": {"ok": False}}
assert panel.refreshed_navigation(dict(held), transient, None) == held
assert panel.refreshed_navigation(dict(held), {"room": {"id": "flow-room"}}, None) == held
assert panel.refreshed_navigation(dict(held), {"room": {"id": "other-room"}}, None) == {"collapsed": set(), "offset": 0}
# An open preview re-reads evidence only when the room moved.
previewed_nav = {"room_id": "flow-room", "tab": "detail", "preview": {"target": ("result", "c1"), "report": {}}}
moved = {"room": {"id": "flow-room", "message_count": 1}}
assert panel.refreshed_navigation(previewed_nav, moved, None)["preview"].pop("refresh") is True
assert "refresh" not in panel.refreshed_navigation(previewed_nav, moved, None)["preview"]
assert panel.refreshed_navigation(previewed_nav, {"room": {"id": "flow-room", "message_count": 2}}, None)["preview"]["refresh"]
# A failed evidence read still names its target, so the error cannot land on another preview.
with patch.object(panel, "call_results", side_effect=subprocess.TimeoutExpired("agent-events", 30)):
    assert panel.read_evidence(project, ("result", "c1"))[0] == ("result", "c1")
    assert panel.read_evidence(project, ("result", "c1"))[2]
# Notices age out after ten seconds.
aging = {"notice": "Opening chat..."}
assert panel.expire_notice(aging, 100.0) is False and panel.expire_notice(aging, 109.0) is False
assert panel.expire_notice(aging, 110.0) is True and "notice" not in aging
aging["notice"] = "again"
assert panel.expire_notice(aging, 111.0) is False and aging["notice"] == "again"
from panel_tree import render_detail
snapshot_task = {"id": "snap", "title": "Snapshot task", "state": "running", "depends": ["base"],
    "provider": "codex", "claimed_at": "2026-10-11T00:00:00Z",
    "verify": "python3 verify.py " + "x" * 300, "allowed_paths": ["scripts/lib/panel.py"]}
snapshot_state = {"room": {"repo_tasks": [snapshot_task]}}
snapshot_nav = {"room_id": "snapshot-room"}
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.room, "status", return_value={"participants": []}):
    panel.navigate(project, ("task", "snap"), snapshot_state, snapshot_nav, 100)
snapshot_text = snapshot_nav["detail"]["text"]
assert "Depends: base" in snapshot_text and "Claimed by: codex / " in snapshot_text and " ago" in snapshot_text
assert "Verify: python3 verify.py" in snapshot_text and "lease_" not in snapshot_text
assert "Scope: scripts/lib/panel.py" in snapshot_text and len(snapshot_text) < 1000
snapshot_nav = {"room_id": "snapshot-room"}
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.room, "status", return_value={"participants": []}):
    panel.navigate(project, ("task", "snap"), {"room": {"repo_tasks": [
        {"id": "snap", "title": "No optional fields", "state": "ready"}]}}, snapshot_nav, 100)
assert "Depends:" not in snapshot_nav["detail"]["text"]
assert "Claimed by:" not in snapshot_nav["detail"]["text"]
assert "Verify:" not in snapshot_nav["detail"]["text"]
long_answer = "\n".join("Answer line %s" % n for n in range(40)) + "\n" + "x" * 900 + " END_OF_ANSWER"
long_result = {"rows": [{"task_id": "long-result", "summary": "Owner summary",
    "calls": [{"kind": "call", "exit": 0, "answer": long_answer,
               "artifact": ".oms/artifacts/long-answer.md"}]}]}
assert "END_OF_ANSWER" in render_results(long_result, 100)
detail = {"title": "RECORDED RESULTS", "report": long_result}
for width in (100, 28, 60):
    screen = render_detail(tree_room, detail, width, 200, {})
    assert "Answer line 39" in screen and "END_OF_ANSWER" in screen, screen
    assert all(display_width(line) <= width for line in screen.splitlines()), screen
# Detail paging must preserve the reader's already wrapped blocks without another clean()/clipped() pass.
from dashboard_projection import ELLIPSIS, use_unicode
previous_unicode = ELLIPSIS[0] == '…'
structured_lines = ['```text', '  a\t=  1  ', '  ' + '한글' * 80 + '  END  ',
                    'stop-reason: literal-data', 'control\x1b[31m\x00', '```', '',
                    '| header | value |', '| --- | --- |', '| repeated  spaces | ' + 'x' * 160 + '  |  ',
                    '', '    indented\tcode  ' + 'z' * 160 + '  ']
structured_source = '\n'.join(structured_lines)
for glyphs in (True, False):
    use_unicode(glyphs)
    marker = '→ ' if glyphs else '-> '
    for width in (24, 50, 80, 100):
        retained = {'rows': [{'summary': structured_source, 'calls': [{'answer': structured_source}]}]}
        expected = render_results(retained, width).split('\n')
        screen = render_detail(tree_room, {'title': 'RECORDED RESULTS', 'report': retained}, width, 1000, {})
        assert screen.split('\n')[2:-2] == expected, (glyphs, width, screen)
        screen = render_detail(tree_room, {'title': 'RAW DETAIL', 'text': structured_source}, width, 1000, {})
        body = screen.split('\n')[2:-2]
        assert all(display_width(line.expandtabs(8)) <= width for line in screen.split('\n')), screen
        cursor = 0
        for original in structured_lines:
            safe = original.replace('\x1b', '?').replace('\x00', '?')
            restored = body[cursor]
            cursor += 1
            while len(restored) < len(safe):
                assert body[cursor].startswith(marker), (width, original, body[cursor])
                restored += body[cursor][len(marker):]
                cursor += 1
            assert restored == safe, (width, restored, safe)
        assert cursor == len(body), body[cursor:]
use_unicode(previous_unicode)
limited_result = deepcopy(long_result)
limited_result["rows"][0]["calls"][0]["answer_truncated"] = True
assert "retained preview limit" in render_results(limited_result, 100)
answer_attempt = {"attempt_id": "att_long_answer", "task_id": "retained-long-answer", "state": "done",
                  "refs": {"panel_room_participant": "long-reader"}}
answer_record = {"kind": "call", "task_id": "retained-long-answer", "attempt_id": "att_long_answer", "exit": 0,
                 "artifact": ".oms/artifacts/long-answer.md"}
sample_auth = "gh" + "p_" + "a" * 25
for payload, truncated in ((long_answer + "\n" + sample_auth + "\n" + "z" * 9000 + " API_ANSWER_TAIL", False),
                           ("x" * 40000 + " API_ANSWER_TAIL", True),
                           ("😀" * 9000 + " TAIL", True),
                           ("😀" * 8191 + "ABCD", False),
                           ("😀" * 8191 + "ABCDE", True),
                           ("to" + "ken=" + "s" * 40000 + " RETAINED_TAIL", False)):
    with patch.object(saved, "_records", return_value=([answer_attempt], [answer_record])), \
            patch.object(saved, "_indexed", return_value=("## Output\n\n" + payload +
                "\n\n## Verify\nPRIVATE_VERIFIER_SENTINEL\n\n## Exit\n\n0\n").encode()):
        normalized = saved.results(project, "retained-long-answer")["rows"][0]["calls"][0]
    assert normalized["answer_truncated"] == truncated, normalized
    assert len(normalized["answer"].encode()) <= saved.MAX_RESULT
    assert sample_auth not in normalized["answer"]
    assert "PRIVATE_VERIFIER_SENTINEL" not in normalized["answer"]
    if not truncated:
        if "Answer line 39" in payload:
            assert "Answer line 39" in normalized["answer"] and "API_ANSWER_TAIL" in normalized["answer"]
        if "RETAINED_TAIL" in payload:
            assert "RETAINED_TAIL" in normalized["answer"] and "s" * 40000 not in normalized["answer"]
    elif "😀" in payload:
        assert "TAIL" not in normalized["answer"]
binding = panel.room_instructions("current-room", "stable-main")
assert "room show --id current-room" in binding and "--room current-room" in binding
assert "room updates --id current-room --participant stable-main" in binding
assert "unacknowledged addressed messages" in binding and "--after CURSOR" in binding
assert "--id current-room --participant stable-main --message MESSAGE_ID" in binding
