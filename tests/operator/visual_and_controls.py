# PANEL_VISUAL_TOKENS_BEGIN
from unittest.mock import patch
from panel_view import (role_style, call_identity, status_tag, paint, GLYPHS, glyph,
                        ROLE_COLORS, STATE_COLORS, box_edge, box_row, use_unicode)
assert role_style("main", "reviewer") == "main"
for role in ("advisor", "reviewer", "critic", "verifier", "security-auditor", "architect", "council", "researcher"):
    assert role_style(role) == role_style("worker", role) == "judge", role
for role in ("worker", "builder", "refactorer", "documenter", "integrator", "performance-engineer", "release-engineer"):
    assert role_style(role) == "worker", role
assert role_style("unrecorded") == "unknown" and role_style("worker", "advise") == "worker"
use_unicode(True)
assert call_identity("worker", "gpt-6.1-sol", "Routing") == "WORKER Sol 6.1 · Routing"
assert call_identity("reviewer", "gpt-6.1-sol", "Goal verification", purpose="verifier") == "VERIFIER Sol 6.1 · Goal verification"
assert call_identity("reviewer", "gpt-6.1-sol", purpose="security-auditor") == "SECURITY AUDITOR Sol 6.1"
assert call_identity("worker", "gpt-6.1-sol", "Routing", alias="B7") == "B7 WORKER Sol 6.1 · Routing"
assert call_identity(None, None, short_id="abcd") == "UNKNOWN model unknown #abcd"
tags = {"starting": "[⟳ starting]", "working": "[⟳ running]", "manual": "[⟳ running]", "verifying": "[⟳ verifying]",
        "idle": "[○ idle]", "blocked": "[! blocked]", "waiting_input": "[? needs input]",
        "waiting_approval": "[! needs approval]", "review": "[! awaiting decision]",
        "unknown": "[? status unknown]", "done": "[✓ finished: exit 0]", "failed": "[✗ failed: exit 1]",
        "timed_out": "[⏱ timed out]", "cancelled": "[⊘ cancelled]", "abandoned": "[✗ abandoned]",
        "orphaned": "[✗ lost process]", "checks_passed": "[✓ checks passed]", "14/14 pass": "[✓ 14/14 pass]",
        "test_failed": "[test failed: exit 1]", "checks_not_run": "[checks not run]",
        "checks_unknown": "[checks unknown]", "acceptance_pending": "[acceptance pending]", "accepted": "[✓ accepted]"}
for state, tag in tags.items():
    code = 0 if state == "done" else 1 if state in {"failed", "test_failed"} else None
    assert status_tag(state, exit_code=code) == tag, state
    ascii_tag = status_tag(state, exit_code=code, unicode=False)
    assert ascii_tag.isascii() and ascii_tag == tag.replace("⟳", "~").replace("○", "o").replace("✓", "+").replace("✗", "!").replace("⏱", "!").replace("⊘", "x"), state
assert status_tag("done") == "[✓ finished]" and status_tag("invented") == "[? status unknown]"
for name in GLYPHS:
    assert glyph(name, False).isascii(), name
for width in (28, 42, 80, 110, 157):
    for unicode in (False, True):
        for dashed in (False, True):
            assert display_width(box_edge("검토 e\u0301", width, unicode, dashed=dashed)) == width
            assert display_width(box_row("한글 e\u0301", width, unicode, dashed=dashed)) == width
with patch.dict(os.environ, {}, clear=True):
    for colors in (16, 256):
        for light in (False, True):
            for token, shades in dict(ROLE_COLORS, **STATE_COLORS).items():
                text = paint("한글", token, color=True, colors=colors, light=light, bold=True)
                sgr = "38;5;%s" % shades[int(light)] if colors == 256 else str(37 if light and token in {"unknown", "dim"} else shades[2])
                assert text == "\033[1;" + sgr + "m한글\033[0m", (token, text)
                assert display_width(re.sub(r"\033\[[0-9;]*m", "", text)) == 4
            assert paint("role", "main", color=True, colors=colors, light=light) != paint("state", "running", color=True, colors=colors, light=light)
    assert paint("plain", "main") == "plain"
    for setting in ({"NO_COLOR": ""}, {"NO_COLOR": "", "OMS_PANEL_COLOR": "always"},
                    {"OMS_PANEL_COLOR": "0"}, {"OMS_PANEL_COLOR": "never"}):
        with patch.dict(os.environ, setting):
            assert paint("plain", "main", color=True, colors=256) == "plain"
use_unicode(False)
assert call_identity("worker", "gpt-6.1-sol", "Routing") == "WORKER Sol 6.1 / Routing"
use_unicode(True)
visual_fixture = {"collection": {"ok": True}, "repo": {"name": "visual"}, "goal": {"text": "한글 목표"},
                  "room": {"id": "visual", "participants": [], "messages": []}, "attempts": {"active_recent": []}}
for ident, role, purpose, state in (("visual-main", "main", None, "working"),
                                  ("visual-judge", "worker", "architect", "working"),
                                  ("visual-held", "reviewer", None, "blocked"),
                                  ("visual-worker", "worker", "builder", "working")):
    visual_fixture["room"]["participants"].append({"participant": ident, "role": role, "joined": True,
        "parent": None if role == "main" else "visual-main", "model": "gpt-6.1-sol", "label": ident})
    visual_fixture["attempts"]["active_recent"].append({"attempt_id": ident, "state": state,
        "parent_attempt_id": None if role == "main" else "visual-main",
        "panel": {"role": role, "purpose": purpose, "room_id": "visual", "room_participant": ident,
                  "model": "gpt-6.1-sol", "label": ident}})
for mode in ("tree", "summary", "list", "compact", "detail"):
    nav = {}
    picture = render(visual_fixture, "codex", 157, 61, view=mode, navigation=nav)
    assert picture.index("ARCHITECT Sol") < picture.index("MAIN Sol") < picture.index("BUILDER Sol"), picture
    assert (picture.index("REVIEWER Sol") < picture.index("ARCHITECT Sol")) == (mode in {"tree", "summary"}), picture
    assert "╎" not in picture, picture
    action = ("result", "visual-worker")
    chosen = render(visual_fixture, "codex", 157, 61, view=mode, navigation={"selected": action})
    assert "╎" in chosen and "visual-worker" in chosen, chosen
    with patch.dict(os.environ, {}, clear=True):
        chosen_color = render(visual_fixture, "codex", 157, 61, view=mode, color=True,
                              navigation={"selected": action})
    assert re.search(r"\033\[[0-9;]*m╎\033\[0m", chosen_color), chosen_color
    if mode in {"tree", "summary"}:
        assert sum(hit["action"] == action and not hit.get("preview") for hit in nav["hits"]) == 1, nav
        assert "╌" in chosen, chosen
    for width, height in ((28, 16), (42, 24), (80, 24), (110, 30), (157, 61)):
        for unicode in (False, True):
            nav = {}
            text = render(visual_fixture, "codex", width, height, view=mode, unicode=unicode, navigation=nav)
            assert len(text.splitlines()) <= height and all(display_width(line) <= width for line in text.splitlines()), text
            assert all(1 <= h["y"] <= len(text.splitlines()) and 1 <= h["x1"] <= h["x2"] <= width for h in nav.get("hits", [])), nav
            if not unicode:
                assert all(ch.isascii() or ch in "한글 목표" for ch in text), text
# PANEL_VISUAL_TOKENS_END

# Judge bands use bounded, cached artifact judgments rather than command exits.
from panel_view import judgment_tag, judge_band, _JUDGMENTS, ROLE_COLORS
import time as judgment_clock
from copy import deepcopy
assert judge_band("worker", "verifier") == "gate" and judge_band("researcher") == "counsel"
assert judge_band("main", "advisor") is None and ROLE_COLORS["counsel"] != ROLE_COLORS["judge"]
judgment_root = temporary / "judgment-board"
judgment_artifacts = judgment_root / ".oms/artifacts"
judgment_artifacts.mkdir(parents=True)
judgment_fixture = {"collection": {"ok": True}, "repo": {"name": "judgments", "path": str(judgment_root)},
    "room": {"id": "judgments", "participants": [], "messages": [], "call_results": {}},
    "attempts": {"active_recent": []}, "goal": {"text": "Review the patch and choose the next step"}}
for ident, role, state, code, answer in (
        ("judge-main", "main", "working", None, None),
        ("gate-pass", "reviewer", "done", 0, "GATE: pass\n> GATE: fail\n```\nGATE: fail\n```"),
        ("gate-fail", "verifier", "failed", 2, "GATE: fail\n- [P1] Missing validation\nP2: Selection\n### P2 Cache\n```\nP0: example\n```\n> P3: quote"),
        ("counsel-verdict", "critic", "failed", 2, "VERDICT: REVISE"),
        ("judge-worker", "worker", "working", None, None)):
    judgment_fixture["room"]["participants"].append({"participant": ident, "role": role, "joined": True,
        "parent": None if role == "main" else "judge-main", "model": "gpt-6.1-sol", "label": ident})
    judgment_fixture["attempts"]["active_recent"].append({"attempt_id": ident, "state": state,
        "panel": {"role": role, "room_id": "judgments", "room_participant": ident, "model": "gpt-6.1-sol"}})
    if answer is not None:
        (judgment_artifacts / (ident + ".md")).write_text("## Output\n" + answer + "\n## Exit\n" + str(code) + "\n")
        judgment_fixture["room"]["call_results"][ident] = {"exit": code, "artifact": ".oms/artifacts/" + ident + ".md",
            "ts": judgment_clock.strftime("%Y-%m-%dT%H:%M:%SZ", judgment_clock.gmtime())}
from room_view import nodes as judgment_nodes
judge_members = {m["participant"]: m for m in judgment_nodes(judgment_fixture)}
assert judgment_tag(judge_members["gate-pass"], judgment_fixture) == "[✓ gate pass]"
assert judgment_tag(judge_members["gate-fail"], judgment_fixture) == "[✗ gate fail · P1 1 · P2 2]"
assert judgment_tag(judge_members["counsel-verdict"], judgment_fixture) == "[verdict revise]"
_JUDGMENTS.clear()
with patch("panel_view.os.open", wraps=os.open) as judge_open:
    for glyphs in (False, True):
        judgment_tag(judge_members["gate-pass"], judgment_fixture, glyphs)
        judgment_tag(judge_members["gate-pass"], judgment_fixture, glyphs)
    assert judge_open.call_count == 1
    (judgment_artifacts / "gate-pass.md").write_text("GATE: fail\nP0: changed\n")
    assert judgment_tag(judge_members["gate-pass"], judgment_fixture) == "[✗ gate fail · P0 1]"
    assert judge_open.call_count == 2
(judgment_artifacts / "gate-pass.md").write_text("GATE: pass\n")
sample_judge = dict(judge_members["counsel-verdict"])
for answer, expected in (("Recommendation: PROCEED", "[verdict proceed]"),
                         ("Recommendation: proceed\nVERDICT: revise", "[verdict revise]"),
                         ("An arbitrary first sentence", "[✗ no verdict (exit 2)]"),
                         ("```\nVERDICT: proceed\n```", "[✗ no verdict (exit 2)]")):
    (judgment_artifacts / "counsel-verdict.md").write_text(answer)
    assert judgment_tag(sample_judge, judgment_fixture) == expected
(judgment_artifacts / "counsel-verdict.md").write_text("VERDICT: revise\n")
assert judgment_tag(dict(sample_judge, role="researcher"), judgment_fixture) == "[answered]"
assert judgment_tag(dict(sample_judge, state="working"), judgment_fixture) == "[⟳ running]"
assert judgment_tag(dict(sample_judge, artifact=".oms/artifacts/missing.md"), judgment_fixture) == "[✗ no verdict (exit 2)]"
assert judgment_tag(dict(sample_judge, exit=0, artifact=".oms/artifacts/missing.md"), judgment_fixture) == "[✗ failed: exit 0]"
assert judgment_tag(dict(sample_judge, artifact=".oms/artifacts/../outside.md"), judgment_fixture) == "[✗ no verdict (exit 2)]"
large_artifact = judgment_artifacts / "large.md"
large_artifact.write_text("GATE: fail\n" + "x" * 70000 + "\nGATE: pass\n")
assert judgment_tag(dict(judge_members["gate-pass"], artifact=".oms/artifacts/large.md"), judgment_fixture) == "[✓ gate pass]"
(judgment_artifacts / "denied.md").write_text("VERDICT: proceed\n")
with patch("panel_view.os.open", side_effect=PermissionError) as denied_open:
    for _ in range(2):
        assert judgment_tag(dict(sample_judge, artifact=".oms/artifacts/denied.md"), judgment_fixture) == "[✗ no verdict (exit 2)]"
    assert denied_open.call_count == 1
if os.name == "posix":
    os.mkfifo(judgment_artifacts / "pipe")
    (judgment_artifacts / "linked.md").symlink_to(large_artifact)
    with patch("panel_view.os.open", side_effect=AssertionError("unsafe artifact was opened")):
        for name in ("pipe", "linked.md"):
            assert judgment_tag(dict(sample_judge, artifact=".oms/artifacts/" + name), judgment_fixture) == "[✗ no verdict (exit 2)]"
for view_mode in ("graph", "tree"):
    for glyphs in (False, True):
        fixture_nav = {"dismissed": True, "expanded_groups": {("review", "judge-main"), ("counsel:review", "judge-main")}}
        drawn_judges = render(judgment_fixture, "codex", 120, 40, view=view_mode, unicode=glyphs, navigation=fixture_nav)
        assert drawn_judges.index("REVIEWER Sol") < drawn_judges.index("CRITIC Sol") < drawn_judges.index("MAIN Sol") < drawn_judges.index("WORKER Sol"), drawn_judges
        for tag in ("gate pass", "gate fail", "P1 1", "P2 2", "verdict revise", "Gate", "Counsel"):
            assert tag in drawn_judges, drawn_judges
        assert glyphs or drawn_judges.isascii(), drawn_judges
        for hit in fixture_nav["hits"]:
            assert 1 <= hit["y"] <= 40 and 1 <= hit["x1"] <= hit["x2"] <= 120, hit
        if view_mode == "tree":
            fold_report = deepcopy(judgment_fixture)
            for attempt in fold_report["attempts"]["active_recent"]:
                if attempt["attempt_id"] in {"gate-pass", "counsel-verdict"}:
                    attempt["state"] = "review"
            fold_nav = dict(fixture_nav)
            render(fold_report, "codex", 120, 40, view="tree", unicode=glyphs, navigation=fold_nav)
            fold_keys = [h["action"][1] for h in fold_nav["hits"] if h["action"][0] == "group"]
            assert ("review", "judge-main") in fold_keys and ("counsel:review", "judge-main") in fold_keys
            folded_counsel = render(fold_report, "codex", 120, 40, view="tree", unicode=glyphs,
                navigation={"dismissed": True, "expanded_groups": {("review", "judge-main")}})
            assert "gate pass" in folded_counsel and "verdict revise" not in folded_counsel, folded_counsel
        for ident in ("gate-pass", "gate-fail", "counsel-verdict", "judge-worker"):
            chosen_nav = dict(fixture_nav, selected=("result", ident))
            chosen_judges = render(judgment_fixture, "codex", 120, 40, view=view_mode, unicode=glyphs, navigation=chosen_nav)
            assert "╎" in chosen_judges if glyphs else ":" in chosen_judges, chosen_judges
            assert any(h["action"] == ("result", ident) for h in chosen_nav["hits"]), chosen_nav
        for columns, rows in ((28, 16), (42, 24), (80, 11), (80, 24), (120, 30)):
            small_nav = {"dismissed": True}
            small_judges = render(judgment_fixture, "codex", columns, rows, view=view_mode, unicode=glyphs, navigation=small_nav)
            assert len(small_judges.splitlines()) <= rows and all(display_width(line) <= columns for line in small_judges.splitlines()), small_judges
            assert any(h["action"] == ("chat", "judge-main") for h in small_nav["hits"]), small_nav
            assert any(h["action"][0] == "result" for h in small_nav["hits"]), small_nav
capture = render_graph(judgment_fixture, 120, 30, unicode=False, navigation={"dismissed": True})
assert len(capture.splitlines()) == 30 and capture.isascii(), capture
(temporary / "judge-board-ascii.txt").write_text(capture + "\n")

# Ending names come from typed result/lifecycle evidence, including a successful worker whose verification failed.
from panel_view import call_ending, ending_tag, call_tone, attention_header
from room_view import execution_tag, end_words, call_classifier, session_log, past_body, past_calls
wrapped_past_call = {"participant": "past-wrap", "role": "worker", "model": "gpt-6-luna",
                     "title": "A very long task title that should wrap below its text column",
                     "state": "done", "exit": 0, "ended": "2026-10-20T00:00:00Z", "seq": 1}
wrapped_past = past_body([wrapped_past_call], 76, 5, {}, {}, True)
wrapped_past_call_index = next(i for i, row in enumerate(wrapped_past) if row[1] == ("result", "past-wrap"))
assert wrapped_past[wrapped_past_call_index + 1][0].startswith(" " * 21), wrapped_past
ending_fixture = {"collection": {"ok": True}, "repo": {"name": "endings"},
    "room": {"id": "endings", "participants": [{"participant": "ending-main", "role": "main", "joined": True,
        "model": "gpt-6.1-sol"}], "messages": [], "call_results": {}},
    "attempts": {"active_recent": [{"attempt_id": "ending-main", "state": "working",
        "panel": {"role": "main", "room_id": "endings", "room_participant": "ending-main"}}]}}
ending_cases = (
    ("cancel-exit", "failed", 143, None, None, None, "cancelled", "[⊘ cancelled]", "dim"),
    ("cancel-record", "cancelled", 1, None, "cancelled_by_owner", ".oms/artifacts/cancel.md", "cancelled", "[⊘ cancelled]", "dim"),
    ("guard-record", "failed", 125, None, "worker_authority_violation", ".oms/artifacts/guard.md", "guard_stop", "[⛔ guard stop]", "alert"),
    ("timeout-exit", "failed", 124, None, None, None, "timed_out", "[⏱ timed out]", "bad"),
    ("verify-timeout", "failed", 0, 124, "verification_failed", ".oms/artifacts/verify.md", "verify_timed_out", "[✗ verify timed out]", "bad"),
    ("verify-result", "failed", 0, 2, None, ".oms/artifacts/verify.md", "verify_failed", "[✗ verify failed]", "bad"),
    ("provider-empty", "failed", 3, None, None, None, "provider_error", "[✗ provider error · exit 3]", "bad"),
    ("failure-output", "failed", 1, None, None, ".oms/artifacts/failure.md", "failed", "[✗ failed: exit 1]", "bad"),
    ("owner-decision", "review", 0, 0, None, ".oms/artifacts/decision.md", None, "[! awaiting decision]", "alert"))
for ident, state, code, verify, reason, artifact, kind, expected, style in ending_cases:
    ending_fixture["room"]["participants"].append({"participant": ident, "role": "worker", "parent": "ending-main",
        "joined": True, "label": ident, "model": "gpt-6.1-sol"})
    ending_fixture["attempts"]["active_recent"].append({"attempt_id": ident, "state": state,
        "reason_code": reason, "task_id": ident,
        "panel": {"role": "worker", "room_id": "endings", "room_participant": ident, "purpose": "builder"}})
    ending_fixture["room"]["call_results"][ident] = {"exit": code, "verify_exit": verify,
        "verify_status": "failed" if verify else "passed" if verify == 0 else "not_run", "artifact": artifact,
        "ts": judgment_clock.strftime("%Y-%m-%dT%H:%M:%SZ", judgment_clock.gmtime())}
    ending_fixture["room"]["messages"].append({"id": "result-" + ident, "sender": ident, "recipient": "ending-main",
        "message_kind": "handoff", "pending_for": ["ending-main"], "ts": ending_fixture["room"]["call_results"][ident]["ts"],
        "text": "Call exit=%s; parent acceptance pending." % code})
# A provider error is named only from its recorded exit, never from a missing artifact.
next(a for a in ending_fixture["attempts"]["active_recent"] if a["attempt_id"] == "provider-empty")["provider_error_exit"] = 3
ending_members = {m["participant"]: m for m in judgment_nodes(ending_fixture)}
for ident, state, code, verify, reason, artifact, kind, expected, style in ending_cases:
    member = ending_members[ident]
    assert call_ending(member) == kind, member
    assert execution_tag(member) == expected, member
    assert call_tone(member) == style, member
    ascii_tag = execution_tag(member, False)
    assert ascii_tag.isascii() and ascii_tag == expected.replace("⊘", "x").replace("⛔", "!").replace("⏱", "!").replace("✗", "!").replace("·", "/"), ascii_tag
    if kind:
        assert end_words(member, "wrong") .startswith(kind.replace("_", " ")), member
    log_report = deepcopy(ending_fixture)
    log_report["room"]["messages"] = [m for m in log_report["room"]["messages"] if m["sender"] == ident]
    log = "\n".join(line for line, _, _ in session_log(log_report, 150, 3))
    if kind:
        assert expected in log and (kind == "failed" or "Failed (exit" not in log), log
for code in (129, 130, 143):
    assert status_tag("failed", exit_code=code) == "[⊘ cancelled]"
assert call_ending(dict(ending_members["verify-result"], state="done")) == "verify_failed"
assert call_ending(dict(ending_members["verify-result"], verify_exit=None, reason_code="verification_failed")) == "verify_failed"
assert call_ending(dict(ending_members["verify-result"], verify_exit=124, reason_code="verification_failed")) == "verify_timed_out"
assert call_ending(dict(ending_members["provider-empty"], artifact=".oms/artifacts/output.md", provider_error_exit=3)) == "provider_error"
assert call_ending(dict(ending_members["cancel-exit"], state="working")) is None
ending_queue = attention_items(ending_fixture)
assert {item["cls"] for item in ending_queue} == {"failed", "guard_stop", "timed_out", "verify_failed", "verify_timed_out", "provider_error", "decide"}, ending_queue
assert len(ending_queue) == 7 and not any("cancel" in str(item["row_id"]) for item in ending_queue), ending_queue
assert all(item["cls"] != "stale" for item in ending_queue)
assert call_classifier(ending_fixture)(ending_members["cancel-exit"]) == "past"
compact_decided = deepcopy(ending_fixture)
compact_decided["finalized"] = {"owner-decision": "completed"}
compact_decided_text = render(compact_decided, "codex", 150, 18, view="compact", attention_only=True)
assert "owner-decision" not in compact_decided_text and "awaiting decision" not in compact_decided_text, compact_decided_text
guard_read = deepcopy(ending_fixture)
next(m for m in guard_read["room"]["messages"] if m["sender"] == "guard-record")["pending_for"] = []
assert any(item["cls"] == "guard_stop" for item in attention_items(guard_read))
guard_read["finalized"] = {"guard-record": "accepted"}
assert not any(item["cls"] == "guard_stop" for item in attention_items(guard_read))
projection_extra = deepcopy(ending_fixture)
projection_extra["attempts"]["active_recent"] = [a for a in projection_extra["attempts"]["active_recent"] if a["attempt_id"] != "guard-record"]
projection_extra["room_attempts"] = {"guard-record": {"attempt_id": "guard-record", "state": "failed",
    "task_id": "guard-record", "reason_code": "worker_authority_violation", "refs": {"provider_error_exit": "125"}}}
projected_guard = next(m for m in judgment_nodes(projection_extra) if m["participant"] == "guard-record")
assert projected_guard["guard"] and projected_guard["provider_error_exit"] == 125, projected_guard
projection_extra["attempts"]["active_recent"].append({"attempt_id": "guard-record", "state": "failed",
    "panel": {"role": "worker", "room_id": "endings", "room_participant": "guard-record"}})
assert next(m for m in judgment_nodes(projection_extra) if m["participant"] == "guard-record")["guard"]
projection_extra["room_attempts"]["guard-record"]["attempt_id"] = "another-attempt"
assert not next(m for m in judgment_nodes(projection_extra) if m["participant"] == "guard-record")["guard"]
operation_verify = deepcopy(ending_fixture)
operation_verify["room"]["call_results"]["verify-result"].pop("verify_exit")
operation_verify["operations"] = {"recent": [{"attempt_id": "verify-result", "task_id": "verify-result",
    "kind": "delegate", "verify_exit": 7}]}
assert next(m for m in judgment_nodes(operation_verify) if m["participant"] == "verify-result")["verify_exit"] == 7
for mode in ("graph", "tree"):
    for glyphs in (False, True):
        nav = {"dismissed": True, "expanded_groups": {("review", "ending-main")}}
        picture = render(ending_fixture, "codex", 150, 50, view=mode, unicode=glyphs, navigation=nav)
        assert "cancel-exit" not in picture and "cancel-record" not in picture, picture
        assert "guard stop" in picture and "verify failed" in picture and "provider error" in picture, picture
        assert all(display_width(line) <= 150 for line in picture.splitlines()), picture
        for ident in ("guard-record", "timeout-exit", "verify-result", "provider-empty", "failure-output"):
            expanded = render(ending_fixture, "codex", 150, 50, view=mode, unicode=glyphs,
                navigation=dict(nav, selected=("result", ident)))
            assert "╎" in expanded if glyphs else ":" in expanded, expanded
            assert ident in expanded, expanded
ending_board = render_graph(ending_fixture, 150, 45, navigation={"dismissed": True})
ending_capture = [next(line for line in ending_board.splitlines() if line.startswith("Attention "))]
ending_capture += [next(line for line in ending_board.splitlines() if ident in line and "[" in line)
                   for ident in ("guard-record", "timeout-exit", "verify-result", "provider-empty", "failure-output", "owner-decision")]
ending_past = past_calls(list(ending_members.values()), call_classifier(ending_fixture))
ending_capture += [text for text, _, _ in past_body(ending_past, 150, 10, {}, {}, True) if "cancel-exit" in text]
(temporary / "ending-kinds-capture.txt").write_text("\n".join(ending_capture) + "\n")
with patch.dict(os.environ, {}, clear=True):
    past_colours = render(ending_fixture, "codex", 150, 50, view="graph", color=True,
        navigation={"dismissed": True, "tab": "past"})
    assert paint("[⊘ cancelled]", "dim", color=True, colors=16) in past_colours, past_colours
    attention_colours = render(ending_fixture, "codex", 150, 50, view="graph", color=True,
        navigation={"dismissed": True, "tab": "needs"})
    assert re.search(r"\033\[33m[^\n]*guard stop", attention_colours), attention_colours
    assert re.search(r"\033\[31m[^\n]*provider error", attention_colours), attention_colours

view = json.loads((temporary / "panel-active.json").read_text())
view["goal"] = {"text": "패널 개선 / show workers and verify results"}
original = json.dumps(view, sort_keys=True)
wide = render(view, "claude", 100, 32, previous="ready")
assert "NEXT MAIN / Claude Code" in wide and "[? status unknown]" in wide, wide
assert "model-for-live-worker" in wide and "role: unrecorded" in wide, wide
assert "not accepted" in wide and "ACCEPT" in wide and "native subagents unobserved" not in wide, wide
assert wide.index("MAIN") < wide.index("OMS activity") < wide.index("[? status unknown]"), wide
assert json.dumps(view, sort_keys=True) == original, "render changed its input records"
for width, height in ((100, 28), (38, 28), (24, 18), (12, 10), (2, 8)):
    for menu in (False, True):
        plain = render(view, "codex", width, height, menu=menu)
        assert "\033" not in plain and len(plain.splitlines()) <= height - int(menu), plain
        assert all(display_width(line) <= width for line in plain.splitlines()), plain
assert "[1] Claude" in render(view, "codex", 38, 28, menu=True)
hostile = json.loads(json.dumps(view))
hostile["delegations"][0]["requested_model"] = "bad\033[2J\u202eforged"
assert "\033" not in render(hostile, "claude", 100, 32)
assert "\u202e" not in render(hostile, "claude", 100, 32)
assert "\033[" in render(view, "claude", 100, 32, color=True)
# A model from a different attempt/provider is never copied into a worker card.
view["delegations"] = []
view["operations"]["recent"] = [{"attempt_id": "unrelated", "kind": "ask", "selected_model": "DO_NOT_ATTRIBUTE"}]
attributed_view = render(view, "codex", 100, 32)
assert "DO_NOT_ATTRIBUTE" in attributed_view.split("OMS activity", 1)[0]
assert "DO_NOT_ATTRIBUTE" not in attributed_view.split("OMS activity", 1)[1]
assert "RESEARCHER model unknown" in render(view, "codex", 100, 32)
view["attempts"]["recent"] = [{"state": "timed_out", "provider": "claude", "tool": "review"}]
assert "[⏱ timed out]" in render(view, "codex", 100, 32)
assert "Worker state unavailable" in render({}, "codex", 100, 32)
assert "Collection degraded" in render({}, "codex", 100, 32)
# Titles occupy their own bright lines; metadata never competes for that space.
title = "작업 내용을 여러 줄로 읽기 쉽게 보여 주고 모델별 호출 흐름을 확인합니다"
parts = wrapped(title, 28, 4)
assert len(parts) > 1 and all(display_width(line) <= 28 for line in parts), parts
assert "..." not in " ".join(parts), parts
from copy import deepcopy
live = deepcopy(view)
live["attempts"]["recent"] = []
live["attempts"]["active_recent"] = [{"attempt_id": "att_display_live", "parent_attempt_id": "att_main",
    "state": "working", "provider": "claude", "tool": "peer-delegate", "task_id": "readable-title",
    "panel": {"role": "worker", "model": "claude-sonnet-5-5", "label": title,
              "access": "write", "location": "worktree:example/wt", "effort": "medium"}}]
first = render(live, "codex", 42, 34, frame=0)
second = render(live, "codex", 42, 34, frame=1)
assert first != second and "[⟳ running]" in first, first
assert "worktree example/wt" in render(live, "codex", 100, 60, navigation={"selected": ("result", "att_display_live")})
# Display hierarchy follows explicit ownership; generic parent links are not
# native-main relationships. Compact and attention views never alter records.
from panel_view import hierarchy, worker_rows, render_results
tree = deepcopy(live)
tree["attempts"]["active_recent"] = [
    {"attempt_id": "att_main", "state": "working", "provider": "codex", "tool": "panel-main",
     "panel": {"role": "main", "model": "gpt-6-sol", "label": "Sol main task"}},
    {"attempt_id": "att_opus", "state": "working", "provider": "claude", "tool": "panel-main",
     "panel": {"role": "main", "model": "claude-opus-5-5", "label": "Opus main task"}},
    *tree["attempts"]["active_recent"],
    {"attempt_id": "att_adv", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "panel": {"role": "advisor", "model": "gpt-6-astra", "label": "Architecture advice"}},
    {"attempt_id": "att_rev", "parent_attempt_id": "att_main", "state": "review", "provider": "claude",
     "panel": {"role": "reviewer", "model": "claude-fable-5-1", "label": "Pending review"}},
    {"attempt_id": "att_other", "parent_attempt_id": "att_opus", "state": "blocked", "provider": "codex",
     "panel": {"role": "worker", "model": "gpt-6-luna", "label": "Opus child blocked"}},
    {"attempt_id": "att_legacy", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "tool": "legacy-unlinked"}]
saved_tree = json.dumps(tree, sort_keys=True)
swapped_tree = deepcopy(tree)
swapped_tree["attempts"]["active_recent"].append(
    {"attempt_id": "att_swap", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "panel": {"role": "worker", "provider": "claude", "model": "claude-sonnet-5-5",
               "fallback": "provider-capacity", "label": "Swapped worker"}})
swapped_row = next(r for r in worker_rows(swapped_tree) if r["work"] == "Swapped worker")
assert swapped_row["provider"] == "claude" and "fallback: provider-capacity" in swapped_row["model"], swapped_row
forest = hierarchy(tree, worker_rows(tree))
assert {r["title"] for r in forest} == {"MAIN Sol 6", "No known main"}, forest
assert "Opus child blocked" in {r["work"] for g in next(r for r in forest if r["title"] == "No known main")["groups"] for r in g[1]}, forest
full_tree = render(tree, "codex", 100, 60, view="detail")
assert full_tree.index("ADVISOR Astra") < full_tree.index("MAIN Sol 6") < full_tree.index("WORKER Sonnet"), full_tree
assert "REVIEWER Fable" in full_tree and "Opus child blocked" in full_tree and "No known main" in full_tree
assert re.search(r"ADVISOR Astra.*\[⟳ running\]", full_tree), full_tree
# A running council groups its seats as one visible debate, not loose advisors.
debate = deepcopy(tree)
seat = {"parent_attempt_id": "att_main", "provider": "claude", "tool": "ask", "task_id": "decide",
        "panel": {"role": "advisor", "model": "claude-fable-5-1", "label": "Pick next work"}}
debate["attempts"]["active_recent"] = debate["attempts"]["active_recent"][:2] + [
    {"attempt_id": "att_council", "parent_attempt_id": "att_main", "state": "working", "provider": "codex",
     "tool": "panel-council", "task_id": "decide", "panel": {"role": "advisor", "label": "Pick next work"}},
    dict(seat, attempt_id="att_seat", state="working")]
debate["attempts"]["recent"] = [dict(seat, attempt_id="att_seat_done", state="done")]
debate_tree = render(debate, "codex", 100, 60, view="detail")
assert debate_tree.count("COUNCIL") == 1 and "Debate: Pick next work" in debate_tree and "Seat answering" not in debate_tree, debate_tree
assert "1 seat(s) answering" in debate_tree and "ADVISORS" not in debate_tree, debate_tree
# One bottom box with tabs (Detail, Plan, Debate, Messages) replaces the d and m views and the messages card.
import panel_debate
import panel_messages
import panel_chats
import room_view
from panel_input import TerminalInput, choose
from unittest.mock import patch
from terminal_panel import tab_event
assert panel_debate._sentence("I'm keeping (a), the `!` marker is cheaper. More.") == "I'm keeping (a), the `!` marker is cheaper."
assert panel_debate._sentence('He said "stop! now" ok. More') == 'He said "stop! now" ok.'
assert [panel_debate.choice(t) for t in ("저는 (a)를 고릅니다.", "B: safest", "A good idea", "Yes, ship", "revise", "none")] == \
    ["(a)", "(b)", None, "yes", "revise", None]
assert TerminalInput().decode(b"0\t") == [("0",), ("tab",)]
assert TerminalInput().decode(b"dm") == [("unbound",), ("m",)]
assert TerminalInput().decode(b"y") == [("unbound",)]
assert TerminalInput().decode(b"cy") == [("c",), ("unbound",)]
assert TerminalInput().decode(b"\033[200~cy\033[201~") == [("paste",)]

paste_start = b"\033[200~"
for split in range(1, len(paste_start)):
    decoder = TerminalInput()
    assert decoder.decode(paste_start[:split]) == [], split
    assert decoder.decode(paste_start[split:] + b"cy\033[201~") == [("paste",)], split

stale_prefix = TerminalInput()
stale_prefix.escape_since = 1.0
with patch("panel_input.time.monotonic", return_value=10.0):
    assert stale_prefix.decode(b"\033[200") == []
assert stale_prefix.escape_since == 10.0, stale_prefix.escape_since
with patch("panel_input.time.monotonic", return_value=10.05), \
        patch("panel_input.select.select", return_value=([], [], [])):
    assert stale_prefix.wait(0) == [] and stale_prefix.pending == b"\033[200"
assert TerminalInput().decode(b"m") == [("m",)]
assert TerminalInput().decode(b"i") == [("i",)], "board i toggles the context headers"
ask = ".oms/artifacts/ask/"
seat_calls = [{"kind": "ask", "selected_model": model, "artifact": ask + "%s-%s-s-1%s.md" % (family, model, suffix), "answer": text}
              for suffix, texts in (("", ("Answer: 저는 (a)를 고릅니다. 이유는 단순합니다.", "Answer: I pick (c) first. Reasons follow.",
                                          "Answer: (b) is safest. More.", "VERDICT: proceed")),
                                    ("-r2", ("Answer: **저는 (a)를 고릅니다.** 그대로 유지합니다.\nFULLTEXT-SOL second line",
                                             "Answer: I keep my main point. I'm keeping (a), the `!` marker is cheaper. More.",
                                             "Answer: (b) stays safest. Still.", "VERDICT: revise\nAnswer: x")))
              for (family, model), text in zip((("codex", "gpt-6-sol"), ("claude", "claude-opus-5-5"), ("claude", "claude-fable-5-1"),
                                                ("codex", "gpt-6-astra")), texts)]
debate_rows = {"rows": [{"title": "Panel attention signal", "summary": "Ship (a)", "outcome": "accepted", "calls": seat_calls}],
               "synthesis": {"path": ask + "_synthesis-s-1.md", "text": "## Prompt\n```\n--- begin conversation context (prior turns) ---\nold turn\n--- end conversation context ---\n```\nQuestion: Should the panel flag attention with a color or a glyph? Pick one. Third.\n"
                             "## Output\n### Agreement\nTests first.\n### Dissent\n- **With Astra:** Order differs.\n- **With Sol:** Scope differs.\n"}}
before = deepcopy(msg_report)
BOX_CHARS = "╭╮│─╰╯•→—▸▾·"


def board(tab, width=80, height=60, **kwargs):
    nav = kwargs.pop("nav", None) or {"selected": ("chat", "m2"), "tab": tab}
    nav["tab"] = tab
    return nav, render(msg_report, "codex", width, height, view="graph", navigation=nav, main_attempt="a2", **kwargs)


# Portrait and short-wide boards keep the fitted reading dock at the bottom.
for width, height in ((40, 90), (60, 80), (120, 20), (127, 50)):
    nav, text = board("messages", width, height)
    lines = text.splitlines()
    assert len(lines) == height and nav["tab_strip_visible"] and nav["tab_body_visible"], (width, height, nav)
    assert nav["box"]["y2"] <= height and nav["box"]["y1"] >= 1, (width, height, nav["box"])
    assert all(1 <= hit["y"] <= height and 1 <= hit["x1"] <= hit["x2"] <= width for hit in nav["hits"]), (width, height)
    assert any(hit["action"] == ("tab", "messages") for hit in nav["hits"]), (width, height)
    dock_body = 12 if height >= 56 else 8 if height >= 40 else 5 if height >= 28 else 2
    expected_dock = (min(dock_body, max(3, nav["dock_content_rows"])) if height >= 28 else dock_body) + 2
    assert nav["box"]["y2"] - nav["box"]["y1"] + 1 >= expected_dock, (width, height, nav["box"])

# Every rendered tab keeps the dock on the bottom rows; the Log tab alone owns the wheel's log range.
for surface in ("graph", "tree"):
    for tab, _ in room_view.TABS:
        for height in (45, 60):
            nav = {"selected": ("chat", "m2"), "tab": tab}
            text = render(msg_report, "codex", 127, height, view=surface, navigation=nav, main_attempt="a2")
            assert nav["tab"] == tab and bool(nav.get("log_box")) == (tab == "log"), (surface, tab, height, text)
            assert "Session log" not in text, (surface, tab, height, text)
            assert nav["box"]["y2"] == height - (2 if surface == "graph" else 1), (surface, tab, height, nav)
            previous = dict(nav["box"])
            render(msg_report, "codex", 127, height, view=surface, navigation=nav, main_attempt="a2")
            assert nav["box"] == previous, (surface, tab, height, nav)
# Narrowing below the dock's minimum width hides it and clears its box, so a hidden tab cannot consume keys.
narrow_nav = {"selected": ("chat", "m2"), "tab": "plan"}
render(msg_report, "codex", 127, 60, view="tree", navigation=narrow_nav, main_attempt="a2")
render(msg_report, "codex", 19, 60, view="tree", navigation=narrow_nav, main_attempt="a2")
assert "box" not in narrow_nav and not narrow_nav.get("tab_strip_visible"), narrow_nav
# The dock fills the rows it is given, whatever its content.
fit_nav = {"tab": "detail"}
for count in (3, 5, 4):
    with patch.object(room_view, "summary_card", return_value=(["row"] * count, {})):
        filled, _ = room_view.tab_box(msg_report, 127, 14, fit_nav, height=60)
    assert len(filled) == 14, (count, filled)
# Measurement cannot consume the newest message's reveal before the fitted page draws it.
new_message_report = deepcopy(msg_report)
new_message_report["room"]["messages"] = msg_log[:1]
new_message_nav = {"tab": "messages", "messages_view": {"filter": 2, "selected": None, "open": None, "scroll": 0}}
first_message_box, _ = room_view.tab_box(new_message_report, 127, 14, new_message_nav, height=60)
new_message_report["room"]["messages"].append(msg_log[1])
next_message_box, _ = room_view.tab_box(new_message_report, 127, 14, new_message_nav, height=60)
assert len(first_message_box) == len(next_message_box) == 14 and "line 1" in "\n".join(next_message_box), next_message_box
# An empty log says so in one line and never creates a log range.
empty_log_report = deepcopy(msg_report)
empty_log_report["room"]["messages"] = []
for surface in ("graph", "tree"):
    empty_nav = {}
    empty_text = render(empty_log_report, "codex", 127, 60, view=surface, navigation=empty_nav, main_attempt="a2")
    assert not empty_nav.get("log_box") and empty_nav["tab"] == "log" and empty_text.count("No session events recorded yet") == 1, (surface, empty_text)
# Opt-in captures use the same canonical report as the layout assertions.
if os.environ.get("OMS_OPERATOR_CAPTURE_DIR"):
    capture_dir = Path(os.environ["OMS_OPERATOR_CAPTURE_DIR"])
    capture_dir.mkdir(parents=True, exist_ok=True)
    for tab in ("detail", "plan"):
        for height in (30, 45, 60):
            nav, text = board(tab, 127, height)
            (capture_dir / ("%s-%s.txt" % (tab, height))).write_text(text + "\n", encoding="utf-8")

# A parentless live worker adds the short-board warning before the dock; it must not restore the blank band.
unlinked = deepcopy(msg_report)
unlinked["room"]["participants"].append({"participant": "unlinked-call", "role": "worker", "joined": True,
                                          "model": "gpt-6-luna", "seq": 4, "provider": "codex"})
unlinked["attempts"]["active_recent"].append({
    "attempt_id": "att_unlinked", "state": "working", "provider": "codex", "tool": "peer-delegate",
    "task_id": "unlinked", "panel": {"role": "worker", "model": "gpt-6-luna", "label": "Unlinked worker",
                                          "room_id": "r", "room_participant": "unlinked-call"}})
unlinked_nav = {"selected": ("chat", "m2"), "tab": "messages"}
unlinked_text = render(unlinked, "codex", 120, 20, view="graph", navigation=unlinked_nav, main_attempt="a2")
unlinked_lines = unlinked_text.splitlines()
assert unlinked_nav["tab_body_visible"] and unlinked_nav["box"]["y2"] - unlinked_nav["box"]["y1"] >= 3, unlinked_nav
assert "no known main" in unlinked_text, unlinked_text
blank_run = longest_blank = 0
for line in unlinked_lines:
    blank_run = blank_run + 1 if not line.strip() else 0
    longest_blank = max(longest_blank, blank_run)
assert longest_blank <= 2, (120, 20, longest_blank, unlinked_lines)


# The strip: stable six tabs, selected brackets and count badges only on mail/history/attention, hits equal drawing at 80 and 157 columns and in ASCII.
for width in (80, 157):
    for glyphs in (True, False):
        nav, text = board("detail", width, 40, unicode=glyphs)
        rows = text.split("\n")
        assert len(rows) == 40 and "RECENT MESSAGES" not in text and "d Debates" not in text and "m Messages" not in text, text
        assert "Quit" in rows[-1] and "Tab Next tab" in rows[-1] or width == 80, rows[-1]
        strip_y = max(h["y"] for h in nav["hits"] if h["action"] == ("tab", "detail"))
        tabs = {h["action"][1]: h for h in nav["hits"] if h["action"][0] == "tab" and h["y"] == strip_y}
        assert list(tabs) == [key for key, _ in room_view.TABS], tabs
        for key, name in (("detail", "Detail"), ("plan", "Plan"), ("debate", "Debate"), ("messages", "Messages")):
            hit = tabs[key]
            seg = rows[hit["y"] - 1][hit["x1"] - 1:hit["x2"]]
            assert seg.strip("[] ").startswith(name) and seg.count("[") == (key == "detail"), (width, glyphs, key, seg)
        strip = rows[tabs["detail"]["y"] - 1]
        assert "[Detail]  Plan  Debate 1  Messages 2  Past 0  Attention" in strip, strip
        assert strip.startswith("╭─ " if glyphs else "+- ") and (glyphs or text.isascii()), strip
# A room without debates omits the tab and cycling skips it; badge counts exclude worker-only and acked mail.
no_debate = deepcopy(msg_report)
no_debate["attempts"]["recent"] = []
no_debate["room"]["messages"] = [
    {"id": "acked", "sender": "m1", "recipient": "m2", "targets": ["m2"], "pending_for": [], "message_kind": "question", "text": "acked"},
    {"id": "worker-mail", "sender": "m1", "recipient": "call", "targets": ["call"], "pending_for": ["call"], "message_kind": "question", "text": "worker only"}]
no_debate_nav = {"tab": "debate"}
no_debate_lines, _ = room_view.tab_box(no_debate, 120, 10, no_debate_nav)
assert "debate" not in no_debate_nav["visible_tabs"] and no_debate_nav["tab"] == "log", no_debate_nav
assert "Debate" not in no_debate_lines[0] and "Messages 0" not in no_debate_lines[0], no_debate_lines
no_debate_nav["surface"] = "graph"
no_debate_nav["tab"] = "plan"
assert tab_event(("tab",), no_debate_nav) and no_debate_nav["tab"] == "messages", no_debate_nav
no_debate["room"]["messages"].append({"id": "need-main", "sender": "call", "recipient": "m2", "targets": ["m2"],
                                      "pending_for": ["m2"], "message_kind": "question", "text": "Decide scope"})
assert len(__import__("panel_messages").needs_main(no_debate)) == 1
no_debate["room"]["pending_messages_truncated"] = True
bounded_badge, _ = room_view.tab_box(no_debate, 120, 10, {"tab": "messages"})
assert "Messages 1+" in bounded_badge[0], bounded_badge
no_debate["room"].pop("pending_messages_truncated")
no_debate["room"]["messages"][-1]["pending_for"] = []
assert not __import__("panel_messages").needs_main(no_debate)
# A recorded automatic judgment note is addressed decision work, even though other worker notes are routine.
judgment_badge = deepcopy(no_debate)
judgment_badge["room"]["participants"].append({"participant": "call", "role": "worker", "joined": True, "parent": "m2"})
judgment_badge["room"]["messages"] = [{"id": "judgment-call", "sender": "call", "recipient": "m2", "targets": ["m2"],
                                        "pending_for": ["m2"], "message_kind": "note", "text": "Review passed; admit or reject"}]
assert len(panel.panel_messages.needs_main(judgment_badge)) == 1
judgment_body = panel.panel_messages.tab_body(judgment_badge, 100, 8, {})
assert any("Review passed" in text for text, _, _ in judgment_body), judgment_body
# Tab cycles Log > Detail > Plan > Debate > Messages and never moves by itself; clicks on a tab name select it.
nav, text = board("detail")
seen = []
for unused in range(7):
    assert tab_event(("tab",), nav)
    seen.append(nav["tab"])
assert seen == ["plan", "debate", "messages", "past", "needs", "log", "detail"], seen
nav, text = board("plan")
render(msg_report, "codex", 80, 60, view="graph", navigation=nav, main_attempt="a2")
assert nav["tab"] == "plan"
hit = next(h for h in nav["hits"] if h["action"] == ("tab", "messages"))
assert tab_event(("click", hit["x1"], hit["y"]), nav) and nav["tab"] == "messages"
assert choose(("click", hit["x1"], hit["y"]), nav) == ("tab", "messages") and nav["selected"] == ("chat", "m2")
assert nav["box"]["y1"] == hit["y"] and msg_report == before
# Plan: no repeated goal; completed, review, running, ready, blocked; narrow metadata is on a second row.
nav, plan = board("plan")
plan_rows = [line for line in plan.split("\n") if "Goal:" in line or " t-" in line]
assert not any("Goal:" in line for line, _ in render_plan_rows(msg_report, {}, 80, True)), plan
assert [row.split(" · ")[0].split()[-1] for row in plan_rows] == ["t-run", "t-rev", "t-rdy", "t-blk"], plan_rows
assert "1 done · 0 cancelled" in plan and "t-ver" not in plan and "Goal:" not in plan and "make d" not in plan and "t-run · running 9h stale · Running one" in plan and "Opus 5.5 · make c" in plan, plan
assert "t-rev · in review · Review one" in plan and "unclaimed · make e" in plan and "codex ·" not in plan and "claude ·" not in plan, plan
activity_tree = render(msg_report, "codex", 120, 80, view="tree", navigation={"dismissed": True})
assert "1 in review" in activity_tree and "Plan tab" in activity_tree and "Review one" not in activity_tree, activity_tree
task_hits = [h["action"] for h in nav["hits"] if h["action"][0] == "task"]
assert task_hits == [("task", t) for t in ("t-run", "t-rev", "t-rdy", "t-blk")], task_hits
# Plan End focuses the closed summary even when the board reaps its own selection on repaint.
assert tab_event(("end",), nav) and nav["plan_pick"] == ("plan-closed", "toggle"), nav
board("plan", nav=nav)
assert tab_event(("enter",), nav) and nav["plan_closed"], nav
_, expanded_plan = board("plan", nav=nav)
# The single newly expanded row keeps the height stable and remains reachable by scrolling.
choose(("scroll", 1, 5, nav["box"]["y1"] + 1), nav)
_, expanded_plan = board("plan", nav=nav)
assert "t-ver · done · Verified one" in expanded_plan, expanded_plan
assert tab_event(("enter",), nav) and not nav["plan_closed"], nav
# Recorded completion metadata is visible only for terminal rows; unknown/malformed kinds stay plain done.
completion_report = deepcopy(msg_report)
completion_report["room"]["repo_tasks"] = [
    {"id": "legacy", "title": "Legacy", "state": "done"},
    {"id": "research", "title": "Research", "state": "done", "completion_kind": "research-accepted"},
    {"id": "main-read", "title": "Main READ", "state": "done", "completion_kind": "main-research-accepted"},
    {"id": "satisfied", "title": "Satisfied", "state": "done", "completion_kind": "satisfied-by"},
    {"id": "running-kind", "title": "Running", "state": "running", "completion_kind": "research-accepted"},
    {"id": "unknown-kind", "title": "Unknown", "state": "done", "completion_kind": "operator-proof"},
    {"id": "bad-kind", "title": "Bad", "state": "done", "completion_kind": {"type": "research-accepted"}},
]
completion_plan = "\n".join(row for row, _ in render_plan_rows(completion_report, {}, 120, True))
assert "legacy · done · Legacy" in completion_plan, completion_plan
assert "research · done (research) · Research" in completion_plan, completion_plan
assert "main-read · done (main research) · Main READ" in completion_plan, completion_plan
assert "satisfied · done (satisfied by) · Satisfied" in completion_plan, completion_plan
assert "running-kind · running · Running" in completion_plan, completion_plan
assert "unknown-kind · done · Unknown" in completion_plan and "bad-kind · done · Bad" in completion_plan, completion_plan
completion_hits = [action for _, action in render_plan_rows(completion_report, {}, 120, True) if action]
assert completion_hits == [("task", task_id) for task_id in ("running-kind", "legacy", "research", "main-read", "satisfied", "unknown-kind", "bad-kind")], completion_hits
assert next(row for row, action in render_plan_rows(completion_report, {}, 120, False) if action and action[1] == "legacy").startswith("+ ")
assert next(row for row, action in render_plan_rows(completion_report, {}, 120, True) if action and action[1] == "legacy").startswith("✓ ")
foot = plan.split("\n")[-1]
assert "wheel Scroll" in foot and "Tab Next tab" in foot and "Enter Chat" not in foot and "Ask advisor" not in foot, foot
assert "✓ t-ver" not in plan and "○ t-rdy" in plan and "✗ t-blk" in plan, plan
assert board("plan", unicode=False)[1].isascii()
# Closed rows fold under one summary; the wheel still pages expanded tasks.
long_report = deepcopy(msg_report)
long_report["room"]["repo_tasks"] = [{"id": "long-%02d" % i, "title": "Plan task %s" % i,
                                       "state": "ready" if i == 44 else "done"} for i in range(45)]
for glyphs in (True, False):
    long_nav = {"selected": ("chat", "m2"), "tab": "plan"}
    picture = render(long_report, "codex", 100, 36, view="graph", navigation=long_nav, unicode=glyphs)
    assert "hidden" not in picture and "oms agent-plan list" not in picture and "44 done" in picture, picture
    summary_hit = next(h for h in long_nav["hits"] if h["action"][0] == "plan-closed")
    assert room_view.plan_event(("click", summary_hit["x1"], summary_hit["y"]), long_nav)
    assert long_nav["plan_closed"]
    picture = render(long_report, "codex", 100, 36, view="graph", navigation=long_nav, unicode=glyphs)
    band = next(b for b in long_nav["bands"] if b["name"] == "detail")
    for unused in range(15):
        choose(("scroll", 3, 5, band["y1"] + 1), long_nav)
    picture = render(long_report, "codex", 100, 36, view="graph", navigation=long_nav, unicode=glyphs)
    assert "long-43" in picture and "hidden" not in picture and "oms agent-plan list" not in picture, picture
    assert len(picture.splitlines()) == 36 and all(display_width(line) <= 100 for line in picture.splitlines())
# Debate: opener and time by board name and local clock, question, one line per seat, stance cut, choice changes only.
nav, text = board("debate")
assert "No debate opened by Opus 5.5 · a main opens one with oms panel --council" in board("debate", nav={"selected": ("chat", "m1")})[1]
empty_foot = board("debate", nav={"selected": ("chat", "m1")})[1].split("\n")[-1]
assert "Tab Next tab" in empty_foot and "Seat" not in empty_foot and "Target" not in empty_foot, empty_foot
render(msg_report, "codex", 80, 60, view="graph", navigation=nav, main_attempt="a2")
nav["debate_view"]["report"] = deepcopy(debate_rows)
nav, text = board("debate", nav=nav)
stamp = room_view.clock_stamp("2026-10-07T10:00:00Z")
flat = " ".join(re.sub("[│|]", " ", line) for line in text.split("\n")).split()
flat = " ".join(flat)
for needle in ("COUNCIL model unknown · Panel attention signal · opened by Sol 6.1 · " + stamp + " · 4 of 4 seats answered · round 2 of 2",
               "Question: Should the panel flag attention with a color or a glyph? Pick one. Third.",
               "COUNCIL Sol 6 — 저는 (a)를 고릅니다.", "COUNCIL Opus 5.5 — I'm keeping (a), the `!` marker is cheaper.", "COUNCIL Fable 5.1 — (b) stays safest.",
               "COUNCIL Astra 6 — revise", "(changed in round 2: (c) → (a))", "(changed in round 2: proceed → revise)",
               "Agreement: Tests first.", "Disagreement: With Astra: Order differs.", "With Sol: Scope differs."):
    assert needle in flat, (needle, flat)
assert panel_debate.handle(("scroll", 20), nav)
decision_page = board("debate", nav=nav)[1]
assert "Decision: Ship (a) (outcome: accepted)" in " ".join(decision_page.replace("│", " ").split()), decision_page
assert flat.count("changed in round") == 2 and "Changed after" not in text and "opened by m2" not in text and "2026-10-07T" not in text, text
assert "FULLTEXT-SOL" not in text and "\x1b" not in text and "Debate •" not in text and "[Debate 1]" in text, text
nav["debate_view"]["report"]["synthesis"]["text"] = "## Output\nplain prose"
nav["debate_view"]["report"]["rows"][0].update(summary=None, outcome=None)
bare = board("debate", nav=nav)[1]
assert "Agreement" not in bare and "Disagreement" not in bare and "Decision: not finalized" in " ".join(bare.replace("│", " ").split()) and "oms panel --finalize" in bare, bare
assert "**" not in text and "begin conversation context" not in text, text
assert "Question: Panel attention signal" in bare
# Enter on a seat shows its full answer in the tab, Esc returns to the summary.
assert panel_debate.handle(("down",), nav) and panel_debate.handle(("up",), nav) and panel_debate.handle(("enter",), nav)
assert "round 2 answer" in board("debate", nav=nav)[1] and "FULLTEXT-SOL second line" in board("debate", nav=nav)[1]
# The full-seat view carries the selected round's truncation flag and source path.
full_nav = deepcopy(nav)
full_nav["debate_view"].update(seat=0, scroll=0)
base_call = dict(seat_calls[0], artifact=ask + "codex-gpt-6-sol-s-1-r1.md", answer="round 1 answer", answer_truncated=True)
round_two = dict(base_call, artifact=ask + "codex-gpt-6-sol-s-1-r2.md", answer="round 2 answer", answer_truncated=False)
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "source artifact:" not in board("debate", 80, 40, nav=full_nav)[1]
round_two["answer_truncated"] = True
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
notice = board("debate", 80, 40, nav=full_nav)[1]
assert "Answer was truncated" in notice and base_call["artifact"] not in notice and round_two["artifact"] in notice, notice
round_two["artifact"] = ask + ("x" * 80 + "/") * 2 + "x" * 120 + "-r2.md"
for glyphs in (True, False):
    notice = board("debate", 76, 40, nav=full_nav, unicode=glyphs)[1]
    assert round_two["artifact"] in "".join(line.strip(" │|") for line in notice.splitlines()), notice
    assert len(notice.splitlines()) == 40 and all(display_width(line) <= 76 for line in notice.splitlines())
round_two["answer_truncated"] = False
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "Answer was truncated" not in board("debate", 80, 40, nav=full_nav)[1]
round_two.pop("answer_truncated")
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, round_two]
assert "Answer was truncated" not in board("debate", 80, 40, nav=full_nav)[1]
same_round_replacement = dict(base_call, answer="replacement answer", answer_truncated=False)
full_nav["debate_view"]["report"]["rows"][0]["calls"] = [base_call, same_round_replacement]
replacement_view = board("debate", 80, 40, nav=full_nav)[1]
assert "replacement answer" in replacement_view and "Answer was truncated" not in replacement_view, replacement_view
# Full answers remain scrollable through the retained source byte bound, including
# the tail of both a multiline answer and one long paragraph.
for answer_tail, answer_text in (("LINE-0499", "x\n" * 499 + "LINE-0499"),
                                 ("PARAGRAPH-END", "word " * 1800 + "PARAGRAPH-END"),
                                 ("LONG-PARAGRAPH-END", "x" * 12000 + "LONG-PARAGRAPH-END")):
    assert len(answer_text.encode("utf-8")) <= panel_results.MAX_RESULT
    full_nav = deepcopy(nav)
    full_nav["debate_view"].update(seat=0, scroll=0)
    full_nav["debate_view"]["report"]["rows"][0]["calls"] = [
        dict(seat_calls[0], artifact=ask + "codex-gpt-6-sol-s-1.md", answer=answer_text, answer_truncated=False)]
    visible = board("debate", 80, 40, nav=full_nav)[1]
    assert answer_tail not in visible, visible
    assert tab_event(("scroll", panel_results.MAX_RESULT), full_nav)
    visible = board("debate", 80, 40, nav=full_nav)[1]
    assert answer_tail in "".join(line.strip(" │") for line in visible.splitlines()), (answer_tail, visible)
    assert len(visible.splitlines()) == 40 and all(display_width(line) <= 80 for line in visible.splitlines())
assert tab_event(("escape",), nav) and nav["debate_view"]["seat"] is None and "FULLTEXT-SOL" not in board("debate", nav=nav)[1]
assert not tab_event(("escape",), nav)
nav["tab"] = "debate"
hit = next(h for h in nav["hits"] if h["action"] == ("seat", 1))
assert tab_event(("click", 5, hit["y"]), nav) and nav["debate_view"]["seat"] == 1
nav["debate_view"]["seat"] = None
ascii_debate = board("debate", nav=nav, unicode=False)[1]
assert not any(ch in ascii_debate for ch in BOX_CHARS), ascii_debate
assert "(changed in round 2: (c) -> (a))" in " ".join(ascii_debate.replace("|", " ").split()), ascii_debate
# Messages: names, local times, unread marks, filters, in-place full text; reading never writes; the dot clears once the Debate tab was seen.
from panel_messages import _rows, readable_text
fenced_message = "before\n```bash\necho `kept`\n```\nafter"
assert readable_text(fenced_message) == fenced_message, readable_text(fenced_message)
aligned_message_report = deepcopy(msg_report)
aligned_message_report["room"]["messages"] = [
    {"id": "aligned-a", "sender": "m1", "recipient": "m2", "message_kind": "note",
     "pending_for": [], "ts": "2026-10-12T19:18:00Z", "text": "ALIGN-A"},
    {"id": "aligned-b", "sender": "漢字送り手", "recipient": "m2", "message_kind": "question",
     "pending_for": [], "ts": "2026-10-12T19:19:00Z", "text": "ALIGN-B"},
]
aligned_message_nav = {"selected": ("chat", "m2"), "tab": "messages",
                       "messages_view": {"filter": 2, "selected": None, "open": None, "scroll": 0}}
aligned_message_text = render(aligned_message_report, "codex", 80, 60, view="graph",
                              navigation=aligned_message_nav, main_attempt="a2")
aligned_message_lines = [line for line in aligned_message_text.splitlines() if "ALIGN-" in line]
assert len(aligned_message_lines) == 2, aligned_message_text
aligned_message_columns = [display_width(line[:line.index("ALIGN-")]) for line in aligned_message_lines]
assert aligned_message_columns == [aligned_message_columns[0]] * 2, (aligned_message_columns, aligned_message_lines)
unread_message = [{"id": "tail-check", "sender": "peer", "recipient": "main",
                   "pending_for": ["main"], "text": "Unread body", "ts": "2026-10-12T00:00:00Z"}]
# The row mark carries the read state (filled/hollow, ASCII */-) instead of a trailing word.
read_message = [dict(unread_message[0], id="tail-read", pending_for=[])]
for glyphs, unread_mark, read_mark in ((True, "●", "○"), (False, "*", "-")):
    line = _rows(unread_message, lambda ident: ident, now=1791763200,
                 readers=("main",), unicode=glyphs)[0]["line"]
    seen = _rows(read_message, lambda ident: ident, now=1791763200,
                 readers=("main",), unicode=glyphs)[0]["line"]
    assert " %s note " % unread_mark in line and "unread" not in line, line
    assert " %s note " % read_mark in seen and unread_mark not in seen, seen
nav, shown = board("messages", 110, 24)
assert "Messages: 2 unread for live participants" in shown and "line 29" in shown and "MAIN Opus 5.5 → MAIN Sol 6.1" in shown and "●" in shown and "· unread" not in shown and "\x1b" not in shown, shown
assert "loading earlier" not in shown, shown
nav["messages_view"]["reading"] = True
assert "loading earlier messages" in board("messages", 110, 30, nav=nav)[1]
nav["messages_view"]["reading"] = False
message_capacity = []
for rows_high in (24, 30, 32, 33, 34, 35, 36, 40):
    sized, messages = board("messages", 110, rows_high, nav={
        "selected": ("chat", "m2"), "messages_view": {"filter": 2, "selected": None, "open": None, "scroll": 0}})
    assert ("Filter: all" in messages or rows_high == 24) and "line 29" in messages, messages
    message_capacity.append(sized["box"]["y2"] - sized["box"]["y1"] + 1)
assert message_capacity == sorted(message_capacity), message_capacity
for rows_high in (21, 22):
    sized, messages = board("messages", 110, rows_high, nav={
        "selected": ("chat", "m2"), "messages_view": {"filter": 2, "selected": None, "open": None, "scroll": 0}})
    assert sized["tab_body_visible"] and "line 29" in messages, messages
assert "  Debate 1  " in board("detail", nav=nav)[1] and "Debate •" not in board("detail", nav=nav)[1]
viewed = board("debate")[0]
assert "Debate •" not in board("detail", nav=viewed)[1] and "  Debate 1  " in board("detail", nav=viewed)[1]
nav["messages_view"]["log"] = msg_log
board("messages", 110, 40, nav=nav)
assert tab_event(("up",), nav) and tab_event(("enter",), nav)
opened_message = board("messages", 80, 60, nav=nav)[1]
assert "END" in opened_message and "paragraph two" in opened_message and opened_message.count("word") > 70, opened_message
assert tab_event(("f",), nav) and nav["messages_view"]["open"] is not None
assert "no call" in nav["notice"] and "open_call" not in nav
assert tab_event(("escape",), nav)
# Esc closes an open message and nothing else; with none open it is left to the board.
assert tab_event(("enter",), nav) and nav["messages_view"]["open"] is not None
assert tab_event(("escape",), nav) and nav["messages_view"]["open"] is None and not tab_event(("escape",), nav)
nav["selected"] = ("chat", "m3")
assert tab_event(("right",), nav)
narrowed = board("messages", 110, 40, nav=nav)[1]
assert "Filter: by call" in narrowed and not nav["message_ids"], narrowed
assert tab_event(("right",), nav)
assert "Filter: all" in board("messages", 110, 40, nav=nav)[1]
assert tab_event(("right",), nav)
assert "Filter: to/from me" in board("messages", 110, 40, nav=nav)[1]
assert board("messages", unicode=False)[1].isascii() and msg_report == before, "reading changed the report"
nav, listed = board("messages", 110, 40)
hit = next(h for h in nav["hits"] if h["action"][0] == "message")
assert listed.split("\n")[hit["y"] - 1].lstrip().startswith("│") and "line" in listed.split("\n")[hit["y"] - 1], listed
assert tab_event(("click", 5, hit["y"]), nav) and nav["messages_view"]["open"] == hit["action"][1]
# Supported room messages keep their tail beyond both former wrapping limits; page keys and the wheel reach it.
for message_text in ("x\n" * 999 + "TAIL", "word " * 790 + "TAIL", "한글 " * 570 + "TAIL"):
    assert len(message_text.encode("utf-8")) <= 4000
    for glyphs in (True, False):
        nav, unused = board("messages", 80, 40, unicode=glyphs)
        nav["messages_view"]["log"] = [dict(msg_log[-1], text=message_text)]
        board("messages", 80, 40, nav=nav, unicode=glyphs)
        assert tab_event(("enter",), nav)
        opened = board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert "TAIL" not in opened, opened
        assert tab_event(("scroll", panel_messages.MAX_TEXT), nav)
        tail = board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert "TAIL" in tail, (len(message_text), glyphs, tail)
        assert len(tail.splitlines()) == 40 and all(display_width(line) <= 80 for line in tail.splitlines())
        assert "\x1b" not in tail and (glyphs or not any(ch in tail for ch in BOX_CHARS)), tail
        last_scroll = nav["messages_view"]["scroll"]
        assert tab_event(("pageup",), nav)
        assert "TAIL" not in board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert tab_event(("pagedown",), nav)
        assert "TAIL" in board("messages", 80, 40, nav=nav, unicode=glyphs)[1]
        assert nav["messages_view"]["scroll"] == last_scroll
        assert tab_event(("escape",), nav) and nav["messages_view"]["open"] is None
assert len(panel_messages.readable_text("x" * (panel_messages.MAX_TEXT + 1))) == panel_messages.MAX_TEXT
assert msg_report == before and msg_log[-1]["text"].startswith("line 29"), "reading changed the report or log"
# Unread totals use the full pending projection, independent of the loaded message window.
import room
from unittest.mock import patch
from room_view import live_unread, nodes, settler
def pending_projection(count, consumed=0):
    rows = [{"thread": "r", "text": "T", "seq": 0, "room_event": {"kind": "created"}}]
    rows += [{"seq": i + 1, "room_event": dict(kind="join", participant=m["participant"],
              provider=m["provider"], role=m["role"], label=m["participant"])} for i, m in enumerate(msg_members)]
    rows += [{"seq": i + 4, "ts": msg_log[-1]["ts"], "text": "Pending %d" % i,
              "room_event": dict(kind="message", id="pending-%d" % i, sender="m3",
                                 recipient="all", message_kind="note")} for i in range(count)]
    rows += [{"seq": count + 4 + i, "room_event": dict(kind="received", participant=p,
              messages=["pending-%d" % n for n in range(consumed)])} for i, p in enumerate(("m1", "m2"))]
    with patch.object(room, "records", return_value=rows):
        projection = room.status(str(temporary), "r")
        log = room.messages(str(temporary), "r")
    return projection, log

many_pending = deepcopy(msg_report)
many_pending["room"], many_log = pending_projection(32, consumed=2)
assert len(many_pending["room"]["messages"]) == 12 and many_pending["room"]["message_count"] == 32
assert many_pending["room"]["pending_count"] == 60
assert many_pending["room"]["pending_messages"] == [
    dict({k: m[k] for k in ("id", "pending_for", "sender", "recipient", "message_kind", "answered", "ts")},
         text=m["text"][:300]) for m in many_log if m["pending_for"]]
assert not many_pending["room"]["pending_messages_truncated"]
readers = room_view.live_readers(many_pending, nodes(many_pending), settler(many_pending)[0])
all_rows = panel_messages._rows(many_log, str, readers=readers, width=123)
expected = sum(row["unread"] for row in all_rows)
assert expected == 30  # broadcasts count once per message, rather than once per recipient
for loaded in (True, False):
    many_nav = {"tab": "messages", "messages_view": {
        "filter": 2, "selected": None, "open": None, "scroll": 0}}
    if loaded:
        many_nav["messages_view"]["log"] = many_log
    many_body = panel_messages.tab_body(many_pending, 123, 40, many_nav)
    tagged = sum(bool(action) and " ● " in line for line, action, _ in many_body)
    assert tagged == (expected if loaded else 12), many_body
    assert live_unread(many_pending, nodes(many_pending), settler(many_pending)[0]) == expected
    many_board = render(many_pending, "codex", 127, 60, view="graph", navigation=many_nav)
    assert "Messages 0" not in many_board and not panel_messages.needs_main(many_pending), (loaded, many_board)
    assert "%s unread for live participants" % expected in many_board, (loaded, many_board)
    assert "%s+ unread" % expected not in many_board, many_board
legacy_pending = deepcopy(many_pending)
for key in ("pending_messages", "pending_messages_truncated"):
    legacy_pending["room"].pop(key)
assert live_unread(legacy_pending, nodes(legacy_pending), settler(legacy_pending)[0]) == 12
legacy_board = render(legacy_pending, "codex", 127, 60, view="graph", navigation={"tab": "messages"})
assert "Messages 12" not in legacy_board and "12 unread for live participants" in legacy_board, legacy_board
for count, expected in ((500, 500), (501, "500+")):
    capped = deepcopy(msg_report)
    capped["room"], _ = pending_projection(count)
    assert len(capped["room"]["pending_messages"]) == 500
    assert capped["room"]["pending_messages_truncated"] == (count > 500)
    assert capped["room"]["pending_messages"][0]["id"] == "pending-%d" % (count - 500)
    assert live_unread(capped, nodes(capped), settler(capped)[0]) == expected
    capped_board = render(capped, "codex", 127, 60, view="graph", navigation={"tab": "messages"})
    assert "Messages %s" % expected not in capped_board and not panel_messages.needs_main(capped), capped_board
    assert "%s unread for live participants" % expected in capped_board, capped_board
# A single main needs questions, not worker chatter or result/assignment echoes.
from room_view import live_readers, live_unread, nodes, settler, who
single = deepcopy(msg_report)
single["attempts"] = {"active_recent": [
    {"attempt_id": "a2", "state": "working", "panel": {"room_id": "r", "room_participant": "m2", "role": "main"}},
    {"attempt_id": "w", "state": "working", "panel": {"room_id": "r", "room_participant": "w", "role": "worker"}},
    {"attempt_id": "rv", "task_id": "settled", "state": "review", "panel": {"room_id": "r", "room_participant": "rv", "role": "reviewer"}}]}
single["finalized"] = {"settled": "accepted"}
# The owner's recorded decision also settles a failed call of that task; an undecided failure stays.
decided_settle = settler(dict(single, finalized={"done-task": "completed"}))[0]
assert decided_settle({"state": "failed", "task_id": "done-task", "participant": "fw", "parent": "m2"})
assert not decided_settle({"state": "failed", "task_id": "open-task", "participant": "fw2", "parent": "m2"})
assert settler(dict(single, finalized={"rejected-task": "failed"}))[0](
    {"state": "failed", "task_id": "rejected-task", "participant": "fw3", "parent": "m2"})
single["room"]["participants"] = [msg_members[1],
    {"participant": "w", "role": "worker", "joined": True, "parent": "m2", "model": "gpt-6.1-sol", "label": "Fix message reader"},
    {"participant": "rv", "role": "reviewer", "joined": True, "parent": "m2", "model": "gpt-6-luna"}]
single_log = [dict(msg_log[0], id=ident, sender=sender, recipient=recipient, targets=[recipient],
                   message_kind=kind, text=text, pending_for=pending)
              for ident, sender, recipient, kind, text, pending in (
                  ("worker-note", "w", "m2", "note", "Routine work", []),
                  ("result-w", "w", "m2", "handoff", "Call exit=0; parent acceptance pending.", []),
                  ("assignment", "m2", "w", "handoff", "Assigned: Fix message reader", []),
                  ("brief", "m2", "rv", "note", "Brief for ended reviewer", ["rv"]),
                  ("status", "m2", "all", "status", "Main status", []),
                  ("question", "w", "m2", "question", "Which interface?", ["m2"]))]
single_log[4]["targets"] = ["m2", "w", "rv"]
single_log[-1].update(recipient="all", targets=["m2", "w", "rv"], pending_for=["m2", "w", "rv"])
single["room"].update(messages=single_log, message_count=len(single_log), pairs=[
    {"recipient": "rv", "pending": 2}, {"recipient": "m2", "pending": 1}, {"recipient": "w", "pending": 1}])
# Keep the settled brief in all, but it must never acquire an unread tag.
# A main's nonroutine brief still needs attention, so bound the default case to the first three required messages.
default_single = deepcopy(single)
default_single["room"]["messages"] = [single_log[0], single_log[1], single_log[-1]]
for width in (50, 80, 127):
    single_nav = {"selected": ("result", "w")}
    body = panel_messages.tab_body(default_single, width, 30, single_nav)
    assert "Filter: needs you (1)" in body[0][0] and single_nav["message_ids"] == ["question"], body
    assert all(display_width(line) <= width for line, _, _ in body), body
    assert "Which" in body[-1][0] and "question" in body[-1][0]
    assert single_nav["message_has_call"] and panel_messages.handle(("f",), single_nav)
    assert single_nav.pop("open_call") == ("result", "w")
    assert panel_messages.handle(("enter",), single_nav)
    assert single_nav["messages_view"]["open"] == "question" and "open_call" not in single_nav
    assert len(panel_messages.tab_body(default_single, width, 30, single_nav)) > len(body)
    assert panel_messages.handle(("f",), single_nav) and single_nav.pop("open_call") == ("result", "w")
    assert panel_messages.handle(("enter",), single_nav)
    assert single_nav["messages_view"]["open"] is None and "open_call" not in single_nav
    assert panel_messages.tab_body(default_single, width, 30, single_nav) == body
    assert panel_messages.handle(("right",), single_nav)
    body = panel_messages.tab_body(single, width, 30, single_nav)
    assert "Filter: by call" in body[0][0] and set(single_nav["message_ids"]) == {"question", "brief"}, body
    worker_line = next(line for line, action, _ in body if action == ("message", "question"))
    assert "running" in worker_line and "+1" in worker_line, body
    single_nav["messages_view"].update(selected="question", tail=False)
    panel_messages.tab_body(single, width, 30, single_nav)
    assert panel_messages.handle(("f",), single_nav) and single_nav.pop("open_call") == ("result", "w")
    assert panel_messages.handle(("right",), single_nav)
    body = panel_messages.tab_body(single, width, 30, single_nav)
    assert "Filter: all" in body[0][0]
    tagged = [line for line, action, _ in body if action and (" ● " in line or " * " in line)]
    members = nodes(single)
    settled, _ = settler(single)
    assert live_readers(single, members, settled) == {"m2", "w"}
    assert live_unread(single, members, settled) == len(tagged) == 1, body
    projected_single = deepcopy(single)
    projected_single["room"]["pending_messages"] = [
        {"id": m["id"], "pending_for": m["pending_for"]} for m in single_log if m["pending_for"]]
    projected_single["room"]["pending_messages_truncated"] = False
    assert live_unread(projected_single, members, settled) == len(tagged) == 1
    projected_single["room"]["pending_messages"] = []
    assert live_unread(projected_single, members, settled) == 0, "empty projection used stale message rows"
    assert not any("unread" in line for line, action, _ in body if action == ("message", "brief"))
    single_nav["messages_view"].update(selected="brief", tail=False)
    panel_messages.tab_body(single, width, 30, single_nav)
    assert not single_nav["message_has_call"]
    assert panel_messages.handle(("f",), single_nav) and "open_call" not in single_nav and "no call" in single_nav["notice"]
    single_nav.pop("notice")
    for opened in ("brief", None):
        assert panel_messages.handle(("enter",), single_nav)
        assert single_nav["messages_view"]["open"] == opened
        assert "open_call" not in single_nav and "notice" not in single_nav
        panel_messages.tab_body(single, width, 30, single_nav)
    assert panel_messages.handle(("right",), single_nav)
    assert "Filter: this call" in panel_messages.tab_body(single, width, 30, single_nav)[0][0]
    assert panel_messages.handle(("right",), single_nav)
    assert "Filter: needs you" in panel_messages.tab_body(single, width, 30, single_nav)[0][0]
    full_board = render(single, "codex", width, 60, view="graph", navigation={"tab": "messages"})
    assert ("Messages 1" if width >= 76 else "Messages: 1 unread") in full_board, full_board
for glyphs in (True, False):
    for help_open in (True, False):
        hint_nav = {"tab": "messages", "tab_strip_visible": True, "message_has_call": True,
                    "keys_help": help_open, "help_rows": 8,
                    "messages_view": {"selected": "question", "open": "question"}}
        hints = " ".join(room_view.footer_hints(hint_nav, 200, True, glyphs, None, True))
        assert "Enter Collapse" in hints and "f Open right" in hints, hints
        hint_nav["messages_view"]["open"] = None
        assert "Enter Expand" in " ".join(room_view.footer_hints(hint_nav, 200, True, glyphs, None, True))
assert who(members[0]) == who(members[0], task=False)
worker = next(m for m in members if m["participant"] == "w")
assert "Fix message reader" in who(worker, task=True) and "Fix message reader" not in who(worker)
worker["title"] = "Task unrecorded"
assert who(worker, task=True) == who(worker)
peer_row = panel_messages._rows([dict(single_log[-1], sender="w", recipient="rv", message_kind="decision")],
                               str, mains=["m2"], width=127)[0]
assert "w → rv" in peer_row["line"] and "decision" in peer_row["line"]
handoff_only = deepcopy(single)
handoff_only["room"]["messages"] = [single_log[1]]
handoff_nav = {"messages_view": {"filter": 1, "selected": None, "open": None, "scroll": 0}}
handoff_body = panel_messages.tab_body(handoff_only, 80, 10, handoff_nav)
handoff_line = next(line for line, action, _ in handoff_body if action == ("message", "result-w"))
assert handoff_nav["message_ids"] == ["result-w"] and "(no notes)" in handoff_line, handoff_body
assert handoff_nav["message_has_call"] and "Finished." not in handoff_line
# Joined non-main members without messages stay visible, but have no selectable message action.
quiet_call = deepcopy(handoff_only)
quiet_call["room"]["participants"].append({"participant": "quiet", "role": "worker", "joined": True,
                                         "parent": "m2", "model": "gpt-6-luna", "label": "Quiet call"})
for width in (24, 50, 80, 127):
    for glyphs in (True, False):
        quiet_nav = {"messages_view": {"filter": 1, "selected": None, "open": None, "scroll": 0}}
        quiet_body = panel_messages.tab_body(quiet_call, width, 30, quiet_nav, glyphs)
        quiet_rows = [(line, selected) for line, action, selected in quiet_body[1:] if action is None]
        # Both the existing reviewer and the new worker have no messages.
        assert len(quiet_rows) == 2 and not any(selected for _, selected in quiet_rows), (width, glyphs, quiet_body)
        assert width < 50 or all("(no notes)" in line for line, _ in quiet_rows), (width, glyphs, quiet_body)
        assert [action for _, action, _ in quiet_body[1:]] == [("message", "result-w"), None, None], quiet_body
        if width >= 80:
            assert "REVIEWER" in quiet_rows[0][0] and "Quiet call" in quiet_rows[1][0], quiet_body
        assert quiet_nav["message_ids"] == ["result-w"] and None not in quiet_nav["message_calls"], quiet_nav
        assert all(display_width(line) <= width for line, _, _ in quiet_body), (width, glyphs, quiet_body)
        empty_call = deepcopy(quiet_call)
        empty_call["room"]["messages"] = []
        empty_nav = {"messages_view": {"filter": 1, "selected": None, "open": None, "scroll": 0}}
        empty_body = panel_messages.tab_body(empty_call, width, 30, empty_nav, glyphs)
        assert len(empty_body) == 4 and all(action is None and not selected for _, action, selected in empty_body), empty_body
        assert not empty_nav["message_ids"] and not empty_nav["message_calls"] and not empty_nav["message_has_call"], empty_nav
        assert all(display_width(line) <= width for line, _, _ in empty_body), (width, glyphs, empty_body)
        for key in ("down", "enter", "f"):
            assert panel_messages.handle((key,), empty_nav)
            assert empty_nav["messages_view"]["open"] is None and "open_call" not in empty_nav, empty_nav
approval = deepcopy(single)
approval["room"]["messages"] = [dict(single_log[-1], id="approval", sender="m2",
    message_kind="question", text="Assigned: approve this change?")]
approval_nav = {}
approval_body = panel_messages.tab_body(approval, 80, 10, approval_nav)
assert approval_nav["message_ids"] == ["approval"] and "approve this change?" in approval_body[-1][0], approval_body
# A known call recipient alone does not turn a question into an assignment handoff.
assert not panel_messages.routine(dict(approval["room"]["messages"][0], recipient="w"),
                                  single["room"], {m["participant"]: m for m in members})
for width in (24, 42, 50, 80, 127):
    for glyphs in (True, False):
        for text in ("approval text " * 20, "ok ✔️ e\u0301 각 \u1160 " * 20):
            narrow = deepcopy(single)
            narrow["room"]["messages"][-1]["text"] = text
            narrow["room"]["participants"][1]["label"] = text
            narrow_nav = {"selected": ("result", "w")}
            for filter_index in range(4):
                narrow_nav["messages_view"] = {"filter": filter_index, "selected": None, "open": None, "scroll": 0}
                body = panel_messages.tab_body(narrow, width, 30, narrow_nav, glyphs)
                assert all(display_width(line) <= width for line, _, _ in body), (width, glyphs, body)
                if filter_index == 2:
                    tagged = [line for line, action, _ in body if action and (" ● " in line or " * " in line)]
                    assert live_unread(narrow, nodes(narrow), settler(narrow)[0]) == len(tagged) == 1, body
                if filter_index == 0:
                    assert panel_messages.handle(("enter",), narrow_nav)
                    expanded = panel_messages.tab_body(narrow, width, 30, narrow_nav, glyphs)
                    assert all(display_width(line) <= width for line, _, _ in expanded), expanded

# The footer lists keys that work: Messages/Debate show their own keys; tree tabs remain reachable.
for name, hint in (("messages", "Message"), ("debate", "Seat")):
    nav, text = board(name, 110, 40, managed=True)
    foot = text.split("\n")[-1]
    assert hint in foot and ("Enter Expand" if name == "messages" else "Enter Open right") in foot and "Ask advisor" not in foot and "Space Pin" not in foot, foot
    assert tab_event(("tab",), nav) and nav["tab"] != name
nav, text = board("detail", 110, 40, managed=True)
assert "Tab Next tab" in text.split("\n")[-1]
from panel_tree import render_tree
tree_nav = {"selected": ("chat", "m2")}
tree_text = render_tree(msg_report, 110, 40, main_attempt="a2", navigation=tree_nav)
assert tree_nav["tab_strip_visible"] and tab_event(("tab",), tree_nav), tree_text
assert tree_nav["primary"] == ("chat", "m2") and tree_nav["cancel_owner"] == {
    "participant": "m2", "attempt": "a2"}, tree_nav
narrow_tree_nav = {}
narrow_tree_text = render(msg_report, "codex", 22, 40, view="graph", navigation=narrow_tree_nav)
assert narrow_tree_nav["surface"] == "tree" and narrow_tree_nav["hits"], narrow_tree_text
assert "OMS dashboard" not in narrow_tree_text and "Graph needs" not in narrow_tree_text, narrow_tree_text
# Without a room or navigation, a narrow graph keeps the recorded calls in the list view.
roomless_call = {"collection": {"ok": True}, "attempts": {"active_recent": [
    {"attempt_id": "roomless-worker", "state": "working", "panel": {
        "role": "worker", "model": "gpt-6-luna", "label": "ROOMLESS CALL"}}]}}
roomless_text = render(roomless_call, "codex", 50, 40, view="graph")
assert "WORKER Luna 6" in roomless_text and "[⟳ running]" in roomless_text, roomless_text
assert "ROOMLESS CALL" in render(roomless_call, "codex", 50, 40, view="list", navigation={"selected": ("result", "roomless-worker")})
roomless_nav = {}
roomless_text = render(roomless_call, "codex", 50, 40, view="graph", navigation=roomless_nav)
assert "WORKER Luna 6" in roomless_text and "surface" not in roomless_nav, roomless_text
# A selection from the inbox is valid only if the current tree still exposes it; visible tree selections remain intact.
stale_inbox_nav = {"selected": ("result", "w1"), "selected_row": 2, "row_ids": ["w1"], "surface": "inbox"}
render_tree(msg_report, 110, 40, navigation=stale_inbox_nav)
assert stale_inbox_nav["selected"] is None and "selected_row" not in stale_inbox_nav and "row_ids" not in stale_inbox_nav, stale_inbox_nav
assert tree_nav["selected"] == ("chat", "m2") and tree_nav["selected"] in tree_nav["items"], tree_nav
last_error = "error: Pi is not installed"
for columns in (57, 42):
    control_tree = render_tree(msg_report, columns, 24, menu=True, previous=last_error)
    board_tree = render_tree(msg_report, columns, 24, menu=False, previous=last_error)
    assert "Last: " + last_error in control_tree and "Last: " + last_error not in board_tree, (columns, control_tree)
    assert len(control_tree.splitlines()) <= 23, control_tree
control_graph = render_graph(msg_report, 80, 39, menu=True, previous=last_error)
board_graph = render_graph(msg_report, 80, 39, menu=False, previous=last_error)
assert "Last: " + last_error in control_graph and "Last: " + last_error not in board_graph, control_graph
assert len(control_graph.splitlines()) <= 38, control_graph
# A single main's long team stays fully reachable when it overflows.
last_team = deepcopy(msg_report)
last_team["room"]["participants"] = [msg_members[0]] + [
    {"participant": "last-worker-%d" % n, "role": "worker", "joined": True, "provider": "codex",
     "model": "gpt-6-luna", "seq": 10 + n, "parent": "m1", "state": "working"} for n in range(6)]
last_team["attempts"]["active_recent"] = [last_team["attempts"]["active_recent"][0]]
last_team["attempts"]["active_recent"] += [
    {"attempt_id": "last-call-%d" % n, "state": "working", "parent_attempt_id": "a1",
     "panel": {"room_id": "r", "room_participant": "last-worker-%d" % n, "role": "worker"}} for n in range(6)]
last_team["attempts"]["recent"] = []
from panel_view import clipped
from dashboard_projection import use_unicode
for glyphs in (True, False):
    use_unicode(glyphs)
    folded_nav = {}
    folded_last = render_tree(last_team, 42, 24, menu=True, previous=last_error, unicode=glyphs, navigation=folded_nav)
    assert all(("result", "last-worker-%d" % n) in folded_nav["items"] for n in range(6)), folded_nav
    assert "Last: " + last_error in folded_last and len(folded_last.splitlines()) <= 23, folded_last
    assert glyphs or folded_last.isascii(), folded_last
# Ordinary errors use one clipped row; only multiline messages and recovery commands wrap.
ordinary_last = "error: native resume requires an existing session; this operation needs Codex CLI (install Codex CLI)"
release_id = "a" + "._:-09AZaz" * 15 + "._:-09AZa"
release_command = "oms panel --release-main " + release_id
assert len(release_id) == 160
release_last = "error: this panel already has a live main or unverified owner; if crashed:\n" + release_command
for renderer, columns in ((render_tree, 28), (render_graph, 76)):
    for glyphs in (True, False):
        use_unicode(glyphs)
        ordinary = renderer(last_team, columns, 24, menu=True, previous=ordinary_last, unicode=glyphs)
        ordinary_rows = ordinary.splitlines()
        assert ordinary_rows[-1] == clipped("Last: " + ordinary_last, columns), ordinary
        assert next(n for n, row in enumerate(ordinary_rows) if row.startswith("Last: ")) == len(ordinary_rows) - 1, ordinary
        for previous in (release_last, release_last.replace("\n", " ")):
            for nav in (None, {"menu_help": True, "notice": "capacity checked"}):
                recovery = renderer(last_team, columns, 16, menu=True, previous=previous,
                                    unicode=glyphs, navigation=deepcopy(nav))
                recovery_rows = recovery.splitlines()
                last_at = next(n for n, row in enumerate(recovery_rows) if row.startswith("Last: "))
                assert len(recovery_rows) <= 15 and all(display_width(row) <= columns for row in recovery_rows), recovery
                assert release_command in "".join(recovery_rows[last_at:]), recovery
        board_last = renderer(last_team, columns, 16, menu=False, previous=release_last, unicode=glyphs)
        assert board_last == renderer(last_team, columns, 16, menu=False, unicode=glyphs), board_last
        for rows_high in (1, 2, 4, 8):
            tiny = renderer(last_team, columns, rows_high, menu=True, previous=release_last, unicode=glyphs)
            assert len(tiny.splitlines()) <= rows_high - 1, tiny
grouped_last = deepcopy(last_team)
grouped_last["attempts"]["active_recent"][-1]["state"] = "review"
grouped_text = render_graph(grouped_last, 76, 24, menu=True, previous=last_error,
                            navigation={"group_tree": True, "expanded_groups": {("review", "m1")}})
assert "Last: " + last_error in grouped_text and len(grouped_text.splitlines()) <= 23, grouped_text
for columns in (42, 76):
    dispatched = render(last_team, "codex", columns, 24, view="graph", menu=True,
                        previous=last_error, navigation={})
    assert "Last: " + last_error in dispatched and len(dispatched.splitlines()) <= 23, dispatched
# Status shares the footer budget; pinned attention, the task tail and dock borders remain reachable.
last_attention = deepcopy(last_team)
last_attention["attempts"]["active_recent"][-1]["state"] = "failed"
for columns, rows_high in ((42, 24), (50, 50)):
    last_nav = {}
    last_picture = render_tree(last_attention, columns, rows_high, menu=True,
                               previous=last_error, navigation=last_nav)
    assert "Attention 1" in "\n".join(last_picture.splitlines()[:10]), last_picture
    plan_row = next(i + 1 for i, line in enumerate(last_picture.splitlines()) if "Plan tab" in line)
    assert any(hit["action"] == ("tab", "plan") and hit["y"] == plan_row for hit in last_nav["hits"]), last_picture
    assert last_nav["tab_strip_visible"] and len(last_picture.splitlines()) <= rows_high - 1, last_picture
    if rows_high == 50:
        assert last_nav["tab_body_visible"] and last_nav["box"]["y2"] - last_nav["box"]["y1"] - 1 >= min(dock_body_rows(rows_high), max(3, last_nav["dock_content_rows"])), last_nav
use_unicode(True)
# Tree and graph share tab navigation; a separate detail screen consumes its own keys.
assert tab_event(("tab",), tree_nav) and not tab_event(("tab",), dict(tree_nav, surface="graph", detail={"target": ("chat", "m2")}))
helped = render_tree(msg_report, 110, 40, navigation=dict(tree_nav, keys_help=True))
assert "Tab Next tab" in helped and "Tab badges" not in helped and "n New main" not in render_tree(msg_report, 110, 40, navigation={"keys_help": True}), helped
# A crowded short tree exposes one live main root and keeps every call, including an earlier main's, reachable.
short_tree = deepcopy(msg_report)
short_tree["room"]["open_questions"] = [{"id": "q1", "sender": "m1", "recipient": "m3", "ts": "2026-10-07T10:00:00Z"}]
short_tree["room"]["participants"] = msg_members + [
    {"participant": "w%d" % n, "role": "worker", "joined": True, "model": "claude-opus-5-5", "seq": 9 + n, "provider": "claude",
     "parent": "m1" if n == 5 else "m3", "state": "working"} for n in range(6)]
short_tree["attempts"]["active_recent"] += [
    {"attempt_id": "w%d" % n, "state": "working", "panel": {
        "room_id": "r", "room_participant": "w%d" % n, "role": "worker"}} for n in range(6)]
one_live_root = {("chat", "m3")}
for ascii_only in (False, True):
    short_nav = {"selected": ("chat", "m1")}
    short_text = render_tree(short_tree, 90, 18, navigation=short_nav, unicode=not ascii_only)
    assert "? 1 open" in short_text and (not ascii_only or short_text.isascii()), short_text
    assert one_live_root == {item for item in short_nav["items"] if item[0] == "chat"}, short_nav
    assert one_live_root <= {h["action"] for h in short_nav["hits"]}, short_text
    assert all(short_nav["positions"][main] < short_nav["viewport"] for main in one_live_root), short_nav
    assert all(("result", "w%d" % n) in short_nav["items"] for n in range(6)), short_nav
# Detail keeps its content after Esc dismissed the preview: the box stays so every tab remains reachable.
nav, dismissed = board("plan", 110, 40, nav={"dismissed": True, "tab": "plan"})
assert "1 done · 0 cancelled" in dismissed and "[Plan]" in dismissed and "RECENT MESSAGES" not in dismissed, dismissed
nav, empty = board("detail", 110, 40, nav={"dismissed": True})
assert "No call selected" in empty and "[Detail]" in empty, empty
# Fit the reading dock before sizing the board; the tab's content owns its height within the terminal cap.




# Calls hang below the main as one tree row each, running first, then failed; rows past the board fold into one
# selectable "N more calls" line, the tree scrolls by wheel, keys and that line, and a selected call is always drawn.
import time as crowd_clock
for crowd_size in (8, 12):
    worker_crowd = crowd(msg_report, 0, crowd_size, parent="a2", prefix="priority")
    for member in worker_crowd["room"]["participants"]:
        if member["role"] == "worker":
            member["joined_at"] = crowd_clock.strftime("%Y-%m-%dT%H:%M:%SZ", crowd_clock.gmtime())
    for attempt in worker_crowd["attempts"]["active_recent"]:
        if attempt["attempt_id"] in ("priority-worker-0", "priority-worker-1", "priority-worker-2"):
            attempt["state"] = "failed"
    running_ids = sorted("priority-worker-%d" % n for n in range(3, crowd_size))
    for columns, rows_high in ((190, 63), (157, 67), (157, 61), (127, 40), (110, 30), (80, 24), (42, 24)):
        for glyphs in (True, False):
            where = (crowd_size, columns, rows_high, glyphs)
            branch, last, marked = ("├╌ ", "└╌ ", "├╌ ▸ ") if glyphs else ("+- ", "`- ", "+- > ")
            more = ("…" if glyphs else "...") + " "

            def tree_board(nav):
                picture = render(worker_crowd, "codex", columns, rows_high, view="graph", navigation=nav,
                                 main_attempt="a2", unicode=glyphs)
                picture_lines = picture.splitlines()
                assert len(picture_lines) == rows_high and all(display_width(line) <= columns for line in picture_lines), (where, picture)
                assert glyphs or picture.isascii(), (where, picture)
                assert all(1 <= hit["x1"] <= hit["x2"] <= columns and 1 <= hit["y"] <= rows_high for hit in nav["hits"]), (where, nav)
                tree_hits = [hit for hit in nav["hits"] if hit.get("tree")]
                rows = [picture_lines[hit["y"] - 1] for hit in tree_hits]
                assert tree_hits and all(hit["x1"] == 1 and hit["x2"] == columns for hit in tree_hits), (where, nav)
                gaps = set(range(tree_hits[0]["y"], tree_hits[-1]["y"] + 1)) - {hit["y"] for hit in tree_hits}
                assert all(any(h["y"] == y and h["action"] == nav.get("selected") for h in nav["hits"]) for y in gaps), (where, nav)
                assert all(display_width(row) == columns for row in rows), (where, rows)
                stem = room_view.layout(columns)[1] + 2
                assert any("MAIN Sol" in row for row in picture_lines[:tree_hits[0]["y"]]), (where, picture)
                assert all(row[stem:].startswith(branch[0]) or hit.get("expanded") for hit, row in zip(tree_hits[:-1], rows[:-1])) and (rows[-1][stem:].startswith(last[0]) or tree_hits[-1].get("expanded")), (where, rows)
                folds = [hit for hit, row in zip(tree_hits, rows) if row[stem + len(branch):].startswith(more)]
                calls = [(hit, row) for hit, row in zip(tree_hits, rows) if hit not in folds]
                return picture_lines, tree_hits, folds, calls, stem

            nav = {"dismissed": True}
            if columns < 76:
                # Below the graph's 76 columns the board is the tree view; every call it draws stays selectable.
                picture = render(worker_crowd, "codex", columns, rows_high, view="graph", navigation=nav,
                                 main_attempt="a2", unicode=glyphs)
                assert nav["surface"] == "tree" and len(picture.splitlines()) <= rows_high, (where, picture)
                assert all(display_width(line) <= columns for line in picture.splitlines()) and (glyphs or picture.isascii()), (where, picture)
                assert all(1 <= hit["x1"] <= hit["x2"] <= columns and 1 <= hit["y"] <= rows_high for hit in nav["hits"]), (where, nav)
                assert all(hit["action"] in nav["items"] for hit in nav["hits"] if hit["action"][0] == "result"), (where, nav)
                continue
            picture_lines, tree_hits, folds, calls, stem = tree_board(nav)
            order = [i[1] for i in nav["items"] if i[0] == "result" and i[1].startswith("priority-worker-")]
            assert order == running_ids + ["priority-worker-%d" % n for n in (0, 1, 2)], (where, nav["items"])
            drawn = [hit["action"][1] for hit, row in calls]
            assert drawn == order[:len(drawn)], (where, drawn, order)
            hidden = crowd_size - len(drawn)
            assert len(folds) == int(bool(hidden)) and (not folds or folds[0] is tree_hits[-1]), (where, picture_lines)
            assert not hidden or ("%s%d more workers" % (more, hidden)) in picture_lines[folds[0]["y"] - 1], (where, picture_lines)
            # Roomy boards draw every call; compact boards may fit more after the Attention strip shrinks.
            if rows_high >= 40:
                assert not hidden, (where, picture_lines)
            assert nav["tab_body_visible"], (where, picture_lines)
            for hit, row in calls:
                assert "WORKER" in row and "Luna" in row and row[stem:].startswith((branch, last)), (where, row)
                assert re.search(r"\[(?:⟳|~) running\]  (<1m|\d+[mhd])$|\[(?:✗|!) failed(?:[:\w ]+)?\]  (<1m|\d+[mhd])$", row), (where, row)
            # Every drawn row opens its call; the fold line opens the next hidden call, which scrolls into view.
            for hit in tree_hits:
                pick = dict(nav, hits=nav["hits"])
                assert choose(("click", columns, hit["y"]), pick) is None, (where, hit)
                assert pick["selected"] == hit["action"] and pick["preview"]["target"] == hit["action"], (where, pick)
                assert hit["action"] in nav["items"], (where, nav["items"])
                again_lines, again_hits, again_folds, again_calls, unused = tree_board(pick)
                chosen_rows = [row for shown, row in again_calls if shown["action"] == hit["action"]]
                assert len(chosen_rows) == 1 and (glyph("selected", glyphs) + " ") in chosen_rows[0], (where, again_lines)
            if not hidden:
                continue
            # The wheel scrolls the tree one row; the selected first row stays drawn at the top edge.
            band = next(b for b in nav["bands"] if b["name"] == "calls")
            assert (band["y1"], band["y2"]) == (tree_hits[0]["y"], tree_hits[-1]["y"]), (where, band)
            wheel = dict(nav, selected=("result", drawn[0]))
            tree_board(wheel)
            choose(("scroll", 1, 2, band["y1"]), wheel)
            assert wheel["band_offsets"]["calls"] == 1, (where, wheel["band_offsets"])
            unused, unused, unused, wheel_calls, unused = tree_board(wheel)
            assert [hit["action"][1] for hit, row in wheel_calls] == [drawn[0]] + order[2:len(wheel_calls) + 1], (where, wheel_calls)
            assert wheel["band_offsets"]["calls"] == 1, (where, wheel["band_offsets"])
            # Keys walk every call in tree order; each selected call is drawn and marked.
            keys = {"dismissed": True}
            tree_board(keys)
            for expected in ["m2"] + order:
                choose(("down",), keys)
                assert keys["selected"][1] == expected, (where, keys["selected"])
                key_lines, unused, key_folds, key_calls, unused = tree_board(keys)
                assert expected == "m2" or [row for hit, row in key_calls if hit["action"][1] == expected], (where, key_lines)
            assert len(key_folds) == 1 and key_calls[-1][0]["action"][1] == order[-1], (where, key_lines)


long_label = "A normal recorded task with sufficient words to wrap to a second line"
for width, height in ((80, 40), (157, 60), (126, 90)):
    baseline = set()
    for advisors in (0, 1, 3):
        for workers in (0, 1, 3):
            for label in (None, long_label):
                report = crowd(msg_report, advisors, workers, label)
                classify = room_view.call_classifier(report)
                settled = any(classify(m) == "past" for m in room_view.nodes(report) if m.get("role") != "main")
                visible_tabs = list(room_view.TABS)
                for tab, tab_name in visible_tabs:
                    for selected in (("chat", "m2"), ("result", "x-advisor-0"), ("result", "x-worker-0")):
                        if selected[1] == "x-advisor-0" and not advisors or selected[1] == "x-worker-0" and not workers:
                            continue
                        for closed in (True, False):
                            for glyphs in (True, False) if label else (True,):
                                nav = {"selected": selected, "tab": tab, **({"dismissed": True} if closed else {})}
                                text = render(report, "codex", width, height, view="graph", navigation=nav, main_attempt="a2", unicode=glyphs)
                                rows = text.split("\n")
                                where = (width, height, advisors, workers, label, tab, selected, closed, glyphs)
                                y1, y2 = nav["box"]["y1"], nav["box"]["y2"]
                                assert len(rows) == height and y2 == height - 2 and max(map(display_width, rows)) <= width, (where, text)
                                assert rows[y1 - 1].startswith("╭─ " if glyphs else "+- ") and "[%s" % tab_name in rows[y1 - 1], (where, text)
                                assert rows[y2 - 1].startswith("╰" if glyphs else "+") and "Quit" in rows[-1], (where, text)
                                assert glyphs or text.isascii(), (where, text)
                                detail_band = next(b for b in nav["bands"] if b["name"] == "detail")
                                assert (detail_band["y1"], detail_band["y2"]) == (y1, y2), (where, detail_band, nav["box"])
                                strip = {h["action"][1]: h["y"] for h in nav["hits"] if h["action"][0] == "tab"}
                                assert strip == {key: y1 for key, _ in visible_tabs}, (where, strip)
                                assert y2 - y1 - 1 >= min(dock_body_rows(height), max(3, nav["dock_content_rows"])), (where, nav)
                                baseline.add((tab, y1, y2))
    # Each tab retains the bottom edge and takes at least the minimum dock.
    for tab, _ in room_view.TABS:
        positions = {(y1, y2) for key, y1, y2 in baseline if key == tab}
        assert all(y2 == height - 2 and 5 <= y2 - y1 + 1 for y1, y2 in positions), (width, height, tab, positions)
# Compact Messages keeps its dock at the bottom and a readable body.
compact_docks = set()
for worker_count in (0, 3):
    for phase in ("working", "done"):
        compact_report = crowd(msg_report, 1, worker_count)
        for attempt in compact_report["attempts"]["active_recent"]:
            if attempt["attempt_id"].startswith("x-worker-"):
                attempt["state"] = phase
        compact_nav = {"selected": ("chat", "m2"), "tab": "messages", "messages_view": {"filter": 2, "selected": None, "open": None, "scroll": 0}}
        compact_text = render(compact_report, "codex", 110, 24, view="graph", navigation=compact_nav, main_attempt="a2")
        compact_docks.add((compact_nav["box"]["y1"], compact_nav["box"]["y2"]))
        assert "Messages: 2 unread for live participants" in compact_text and "line 29" in compact_text, compact_text
        assert len(compact_text.splitlines()) == 24 and all(display_width(line) <= 110 for line in compact_text.splitlines()), compact_text
assert all(y2 == 22 and y2 - y1 + 1 >= 4 for y1, y2 in compact_docks), compact_docks
# A main with an unanswered question shows "? N open" on its card; the person sees it in "Needs you" from 20 minutes.
import panel_view as question_view
import time as question_time
asked_report = deepcopy(msg_report)
asked_report["room"]["open_questions"] = [{"id": "q1", "sender": "m1", "recipient": "m2", "ts": question_time.strftime(
    "%Y-%m-%dT%H:%M:%SZ", question_time.gmtime(question_time.time() - 25 * 60))}]
for ascii_only in (False, True):
    asked_board = render(asked_report, "codex", 140, 40, view="graph", navigation={"dismissed": True}, unicode=not ascii_only,
                         main_attempt="a2")
    assert "? 1 open" in asked_board, asked_board
    assert not ascii_only or asked_board.isascii(), asked_board
assert "? 1 open" not in render(msg_report, "codex", 140, 40, view="graph", navigation={"dismissed": True}, main_attempt="a2")
waiting = [i for i in question_view.inbox_items(asked_report) if i["glyph"] == "?"]
assert [(i["who"], i["what"], i["action"]) for i in waiting] == [("Question from Opus 5.5", "to Sol 6.1", ("chat", "m2"))], waiting
# Board/dock v1: topology, evidence lanes, reader ownership and long-goal recovery.
board_fixture = crowd(msg_report, 4, 12, parent="a2", prefix="board")
board_fixture["plan"] = {"present": True, "goal": "Unify the board and keep evidence distinctions " * 8 + "GOAL END",
                         "task_count": 4, "by_state": {"done": 1, "review": 1, "running": 2}}
board_fixture["provider_status"] = {"codex": {"participant": "m2", "context_left": 39, "weekly_used": 72}}
board_fixture["land"] = {"active": True, "sha": "abcdef0123", "step": "ci"}
board_fixture["room"]["statuses"] = {"m2": {"text": "Checking the board", "ts": "2026-10-13T12:00:00Z"}}
board_fixture["room"]["messages"] += [dict(msg_log[-1], id="recorded-outcome", text="Recorded check finished", outcome="finished: exit 0")]
board_fixture["room"]["messages"] += [dict(board_fixture["room"]["messages"][-1])]
for columns, rows_high in ((157, 61), (110, 30), (80, 24)):
    for glyphs in (True, False):
        visual_nav = {"dismissed": True}
        visual = render_graph(board_fixture, columns, rows_high, navigation=visual_nav, main_attempt="a2", unicode=glyphs)
        visual_rows = visual.splitlines()
        assert len(visual_rows) == rows_high and all(display_width(line) <= columns for line in visual_rows), visual
        assert "WORK STATUS" not in visual and "MAIL /" not in visual and "Team:" not in visual, visual
        assert "ctx 61% used" in visual and "ctx 61% left" not in visual, visual
        assert visual.index("ADVISOR Astra") < visual.index("MAIN Sol 6.1") < visual.index("WORKER Luna"), visual
        assert visual_rows[2].startswith("╭─ Goal" if glyphs else "+- Goal") and "1/4 verified" in visual_rows[2], visual
        assert "LANDING" not in "\n".join(visual_rows[:8]) and "LANDING abcdef0 ci" in visual_rows[-2], visual
        assert "Workers 12/12" in visual_rows[-2] and "Judges 4/4" in visual_rows[-2] and "Mail" in visual_rows[-2], visual
        assert "Keys" in visual_rows[-1], visual
        assert sum(line.startswith("Attention ") for line in visual_rows) == 1, visual
        strip = [h for h in visual_nav["hits"] if h["action"][0] == "tab" and h["y"] == visual_nav["box"]["y1"]]
        assert [h["action"][1] for h in strip] == visual_nav["visible_tabs"], strip
        assert "more counsel" in visual or sum(h.get("tree", False) and h["action"][1].startswith("board-advisor") for h in visual_nav["hits"]) == 4, visual
        if rows_high == 61:
            assert "GOAL END" in visual and "Session log" not in visual and visual_nav["tab"] == "log", visual
            assert visual.count("Recorded check finished") == 1 and "[main Opus 5.5]" in visual, visual
        if rows_high == 24:
            assert "Session log" not in visual and ("GOAL END" in visual or "Goal lines" in visual), visual
        assert glyphs or visual.isascii(), visual
# The complete objective survives the board's height limit and the Plan reader's scrolling.
long_objective = deepcopy(board_fixture)
long_objective["plan"]["goal"] = "many words " * 800 + "LAST GOAL WORD"
long_goal_nav = {"tab": "plan", "dismissed": True}
long_screen = render_graph(long_objective, 110, 30, navigation=long_goal_nav, main_attempt="a2")
assert "Goal lines" in long_screen and "full goal in Plan" in long_screen, long_screen
goal_band = next(b for b in long_goal_nav["bands"] if b["name"] == "goal")
choose(("scroll", 1, 5, goal_band["y1"] + 1), long_goal_nav)
scrolled_goal = render_graph(long_objective, 110, 30, navigation=long_goal_nav, main_attempt="a2")
assert "Goal lines 2-" in scrolled_goal and long_goal_nav["band_offsets"]["goal"] == 1, scrolled_goal
long_goal_nav["band_offsets"]["goal"] = len(room_view.goal_box(long_objective, 108, True))
last_goal = render_graph(long_objective, 110, 30, navigation=long_goal_nav, main_attempt="a2")
assert "LAST GOAL WORD" in last_goal and "Goal:" not in "\n".join(last_goal.splitlines()[long_goal_nav["box"]["y1"]:]), last_goal
# A long goal and both judge bands cannot consume the dock's three-row minimum.
dense_goal = deepcopy(board_fixture)
dense_goal["plan"]["goal"] *= 50
next(m for m in dense_goal["room"]["participants"] if m["role"] == "advisor")["role"] = "reviewer"
for height in (28, 30, 45, 60):
    for tab, _ in room_view.TABS:
        dense_nav = {"tab": tab, "dismissed": True}
        render_graph(dense_goal, 127, height, navigation=dense_nav, main_attempt="a2")
        assert dense_nav["box"]["y2"] - dense_nav["box"]["y1"] - 1 >= 3, (height, tab, dense_nav)
# The Log tab fills the dock and keeps an older view anchored as new events arrive.
log_board = deepcopy(board_fixture)
log_board["room"]["messages"] = [dict(board_fixture["room"]["messages"][-1], id="log-%02d" % index,
    text="Session event %02d" % index, ts="2026-10-13T12:00:%02dZ" % index) for index in range(40)]
log_nav = {"dismissed": True}
log_tall = render_graph(log_board, 120, 61, navigation=log_nav, main_attempt="a2", unicode=False)
assert log_nav["tab"] == "log" and "[Log]" in log_tall and "Session log" not in log_tall, log_tall
assert log_nav["log_box"]["y2"] - log_nav["log_box"]["y1"] + 1 > 12, log_nav
assert log_nav["log_box"]["y1"] == log_nav["box"]["y1"] + 1 and log_nav["log_box"]["y2"] == log_nav["box"]["y2"] - 1, log_nav
assert "Session event 39" in log_tall and "Session event 00" not in log_tall, log_tall
(temporary / "session-log-tall.txt").write_text(log_tall + "\n")
log_board["room"]["messages"].append(dict(log_board["room"]["messages"][-1], id="log-40",
    text="Session event 40", ts="2026-10-13T12:00:18Z"))
log_live_board = render_graph(log_board, 120, 61, navigation=log_nav, main_attempt="a2", unicode=False)
assert log_nav["log_offset"] == 0 and "Session event 40" in log_live_board and "Session event 05" not in log_live_board, log_live_board
assert tab_event(("[",), log_nav) and log_nav["log_offset"] == 3
log_scrolled = render_graph(log_board, 120, 61, navigation=log_nav, main_attempt="a2", unicode=False)
assert "[Log ^3]" in log_scrolled and "Session event 37" in log_scrolled and "Session event 40" not in log_scrolled, log_scrolled
(temporary / "session-log-scrolled.txt").write_text(log_scrolled + "\n")
log_board["room"]["messages"].append(dict(log_board["room"]["messages"][-1], id="log-41",
    text="Session event 41", ts="2026-10-13T12:00:19Z"))
anchored = render_graph(log_board, 120, 61, navigation=log_nav, main_attempt="a2", unicode=False)
assert log_nav["log_offset"] == 4 and "Session event 37" in anchored and "Session event 38" not in anchored, anchored
box = log_nav["log_box"]
assert tab_event(("scroll", -2, 20, box["y1"],), log_nav) and log_nav["log_offset"] == 6
log_nav["log_offset"] = 3
log_nav["tab"] = "messages"
assert not tab_event(("[",), log_nav) and log_nav["log_offset"] == 3
assert tab_event(("scroll", -2, 20, box["y2"]), log_nav) and log_nav["log_offset"] == 5
assert not tab_event(("scroll", -2, 20, log_nav["box"]["y1"] - 1), log_nav) and log_nav["log_offset"] == 5
log_nav["tab"] = "log"
log_nav["log_offset"] = 3
assert tab_event(("]",), log_nav) and log_nav["log_offset"] == 0
log_nav["log_offset"] = 999
clamped = render_graph(log_board, 120, 61, navigation=log_nav, main_attempt="a2", unicode=False)
visible_events = log_nav["log_box"]["y2"] - log_nav["log_box"]["y1"] + 1
assert log_nav["log_offset"] == len(log_board["room"]["messages"]) - visible_events, log_nav
assert "Session event %02d" % (visible_events - 1) in clamped and "Session event 40" not in clamped, clamped
# A 157x58 board has no blank gap between the tree and the dock, and the Log tab is first and active by default.
for surface in ("tree", "graph"):
    gap_nav = {}
    gap_screen = render(board_fixture, "codex", 157, 58, view=surface, navigation=gap_nav, main_attempt="a2")
    gap_lines = gap_screen.splitlines()
    assert gap_nav["tab"] == "log" and gap_nav["visible_tabs"][0] == "log" and "[Log]" in gap_lines[gap_nav["box"]["y1"] - 1], (surface, gap_screen)
    assert "Session log" not in gap_screen and gap_nav["box"]["y2"] >= 58 - 3, (surface, gap_nav, gap_screen)
    blank_run = longest = 0
    for line in gap_lines[:gap_nav["box"]["y1"] - 1]:
        blank_run = 0 if line.strip() else blank_run + 1
        longest = max(longest, blank_run)
    assert longest <= 2, (surface, longest, gap_screen)
# A 28-row tree keeps its activity rows; the dock only takes what is left after them.
fit_board = deepcopy(board_fixture)
fit_board["room"]["messages"] = [dict(board_fixture["room"]["messages"][-1], id="fit-%02d" % index,
    text="Fit event %02d" % index, ts="2026-10-13T12:00:%02dZ" % index) for index in range(11)]
for tight_height in (28, 30):
    tight_screen = render(fit_board, "codex", 157, tight_height, view="tree", navigation={"tab": "detail"}, main_attempt="a2")
    assert "MAIN" in tight_screen, (tight_height, tight_screen)
# Only recorded result evidence drives verify/review/acceptance; partial previews warn before all judgments.
summary_result = {"rows": [{"task_id": "summary-task", "outcome": None, "verification": {"status": "passed", "exit": 0},
                           "calls": [{"kind": "delegate", "exit": 0, "answer_truncated": True, "artifact": "recorded.md",
                                      "answer": "## What changed\nFixed the bounded board.\n## Remaining uncertainty\nNative palettes untested.",
                                      "changes": {"count": 2, "added": 10, "removed": 3}},
                                     {"kind": "review", "exit": 0, "answer": "GATE: pass\nGATE: fail"}]}]}
summary_rows, _ = room_view.summary_card(summary_member, {"report": summary_result}, 180)
assert any("partial answer" in line for line in summary_rows), summary_rows
assert "Artifact: recorded.md" in summary_rows, summary_rows
assert not any("not recorded" in line or "unknown" in line or line.startswith(("Changes:", "Author-reported", "Verify:")) for line in summary_rows), summary_rows
staged = deepcopy(summary_result)
staged["rows"][0].update(stages={"implement": {"state": "passed"}, "verify": {"state": "failed"},
                                  "review": {"state": "not_run"}, "main_decision": {"state": "pending"}}, next_action="retry verify")
staged["rows"][0]["calls"][0]["patch"] = "worker.patch"
staged_rows, _ = room_view.summary_card(dict(summary_member, provider="codex", effort="high", tier="routine"), {"report": staged}, 180)
assert "Stages: implement: passed / verify: failed / review: not_run / main decision: pending" in staged_rows, staged_rows
assert "Next: retry verify" in staged_rows and "Patch: worker.patch" in staged_rows, staged_rows
assert "Route: codex / gpt-6.1-sol / high / routine" in staged_rows, staged_rows
ended_summary, _ = room_view.summary_card(dict(summary_member, ended="2026-10-13T12:05:00Z"), {"report": staged}, 180)
assert "Elapsed: 5m" in ended_summary, ended_summary
missing_fields, _ = room_view.summary_card({"participant": "minimal-call", "role": "worker", "state": "working"}, {}, 180)
assert len(missing_fields) == 1 and "unknown" not in missing_fields[0] and "not recorded" not in missing_fields[0], missing_fields
other_task, _ = room_view.summary_card(dict(summary_member, task_id="other-task"), {"report": staged}, 180)
assert not any(line.startswith(("Stages:", "Next:", "Artifact:", "Patch:")) for line in other_task), other_task
exit_only, _ = room_view.summary_card(review_member, {"report": {"rows": [{"task_id": "summary-task", "calls": [{"exit": 0}]}]}}, 160)
assert not any("gate pass" in line or "checks passed" in line for line in exit_only), exit_only
main_summary_report = {"plan": {"tasks": [{"id": "main-task", "title": "Main fix", "state": "running", "claimed_by": "summary-main"}]},
                       "room": {"statuses": {"summary-main": {"text": "Implementing dock"}},
                                "messages": [{"sender": "summary-main", "message_kind": "decision", "text": "Keep fixed height"}]},
                       "land": {"active": True, "sha": "123456789", "step": "gate"},
                       "provider_status": {"codex": {"participant": "summary-main", "context_left": 60}}}
main_member = {"participant": "summary-main", "role": "main", "provider": "codex", "model": "gpt-6.1-sol", "state": "working"}
main_lines, _ = room_view.summary_card(main_member, {}, 180, main_summary_report["room"], report=main_summary_report,
                                      everyone={"summary-main": [dict(summary_member, state="working"), dict(summary_member, state="review")]},
                                      classify=lambda c: "review" if c["state"] == "review" else "live")
assert "Task: main-task · Main fix" in main_lines and "Now: Implementing dock" in main_lines, main_lines
assert "Workers running: 1 · Awaiting decision: 1" in main_lines and "Context used: 40%" in main_lines, main_lines
assert "Land: 123456789 · gate" in main_lines and "Last owner decision: Keep fixed height" in main_lines, main_lines
assert not any(line.startswith(("Verify:", "Review:", "Main decision:", "Author-reported", "Changes:", "Route:")) for line in main_lines), main_lines
# Detail keeps its three-row minimum while the right reader owns a call.
reader_call = ("result", "board-worker-0")
for surface in ("graph", "tree"):
    renderer = render_graph if surface == "graph" else render_tree
    reader_nav = {"tab": "detail", "selected": reader_call, "preview": {"target": reader_call, "report": {"rows": []}},
                  "reader_open": True, "reader_target": reader_call}
    same = renderer(board_fixture, 157, 61, navigation=reader_nav, main_attempt="a2")
    assert reader_nav["tab_strip_visible"] and reader_nav["tab_body_visible"] and reader_nav["box"]["y2"] - reader_nav["box"]["y1"] - 1 >= min(dock_body_rows(61), max(3, reader_nav["dock_content_rows"])), (surface, reader_nav)
    assert [h["action"][1] for h in reader_nav["hits"] if h["action"][0] == "tab" and h["y"] == max(
        t["y"] for t in reader_nav["hits"] if t["action"][0] == "tab")] == reader_nav["visible_tabs"], reader_nav
    reader_nav["reader_target"] = ("result", "board-worker-1")
    different = renderer(board_fixture, 157, 61, navigation=reader_nav, main_attempt="a2")
    assert reader_nav["box"]["y2"] - reader_nav["box"]["y1"] - 1 >= 3 and "reader: board-worker-1 ≠ selected" in different and "Next: check selected evidence" in different, (surface, different)
# Per-tab/per-call scroll is local presentation state and survives tab switches.
scroll_nav = {"tab": "plan", "preview": {"target": reader_call, "report": summary_result}, "band_offsets": {"detail": 2}}
render_graph(board_fixture, 157, 61, navigation=scroll_nav, main_attempt="a2")
plan_offset = scroll_nav["band_offsets"]["detail"]
scroll_nav["tab"] = "detail"
render_graph(board_fixture, 157, 61, navigation=scroll_nav, main_attempt="a2")
scroll_nav["tab"] = "plan"
render_graph(board_fixture, 157, 61, navigation=scroll_nav, main_attempt="a2")
assert scroll_nav["band_offsets"]["detail"] == plan_offset, scroll_nav

# Two recorded mains on one model stay apart in the control window, which reuses the board's names.
twin = deepcopy(asked_report)
twin["room"]["participants"][1]["model"] = twin["room"]["participants"][0]["model"]
twin["room"]["messages"] = [{"id": "bc", "sender": "m1", "recipient": "all", "text": "heads up", "pending_for": ["m2"], "ts": "2026-10-07T10:00:00Z"}]
twin_who = [i["who"] for i in question_view.inbox_items(twin) if i["what"].startswith("to all")]
assert twin_who == ["Opus 5.5 #m1 main"], twin_who
asked_report["room"]["open_questions"][0]["ts"] = question_time.strftime("%Y-%m-%dT%H:%M:%SZ", question_time.gmtime(question_time.time() - 19 * 60))
assert not [i for i in question_view.inbox_items(asked_report) if i["glyph"] == "?"], "under twenty minutes the person is not pulled in"
empty_calls = deepcopy(tree)
empty_calls["attempts"]["active_recent"] = empty_calls["attempts"]["active_recent"][:2]
idle_mains = render(empty_calls, "codex", 100, 40)
assert "MAIN Sol 6" in idle_mains and "MAIN Opus 5.5" not in idle_mains
assert "No active child calls" in idle_mains
own_tree = render(tree, "codex", 100, 60, main_attempt="att_main", view="detail")
assert "Sol main task" in own_tree and "Opus child blocked" not in own_tree and "legacy-unlinked" not in own_tree
compact_tree = render(tree, "codex", 100, 60, view="compact")
assert "Architecture advice" in compact_tree and "worktree example/wt" not in compact_tree
assert "[⟳ running]" in compact_tree and "[⟳ running]" in full_tree
attention_tree = render(tree, "codex", 100, 60, attention_only=True)
assert "Pending review" in attention_tree and "Opus child blocked" in attention_tree
assert "Architecture advice" not in attention_tree and "filtered" in attention_tree
assert json.dumps(tree, sort_keys=True) == saved_tree
# Closed root cards replace deep indentation; role counts and attention titles
# stay readable in the short menu, with stable roots as child states change.
short_tree = render(tree, "codex", 100, 28)
assert "Opus child blocked" in short_tree and "REVIEWER Fable" in short_tree, short_tree
assert "WORKER Sonnet" in short_tree and "No known main" in short_tree, short_tree
assert "2 advisor" not in short_tree
changed_tree = deepcopy(tree)
changed_tree["attempts"]["active_recent"][4]["state"] = "blocked"
changed_text = render(changed_tree, "codex", 100, 60, view="detail")
assert full_tree.index("MAIN Sol 6") < full_tree.index("No known main")
assert changed_text.index("MAIN Sol 6") < changed_text.index("No known main")
missing_model = deepcopy(tree)
missing_model["attempts"]["active_recent"][0]["panel"].pop("model")
missing_text = render(missing_model, "codex", 42, 34, main_attempt="att_main")
assert "MAIN mo…" in missing_text and "Sol 6 / high / working" not in missing_text, missing_text
assert "MAIN model unknown" in render(missing_model, "codex", 100, 34, main_attempt="att_main")

# Usage is the recent artifact window, never an account total or an overlapping
# lifecycle sum. Mixed routes and selected-only identities stay separate.
from dashboard_projection import project_acceptance
malformed_acceptance = project_acceptance({"healthy": True, "criteria": [
    {"id": "malformed", "status": ["bad"]}, {"id": "valid", "status": "failed"}]})
assert [c["id"] for c in malformed_acceptance["criteria"]] == ["valid", "malformed"]
from panel_view import model_usage, usage_labels
usage_view = deepcopy(tree)
usage_view["operations"] = {"available": True, "recent": [
    {"event_id": "use1", "kind": "ask", "provider": "codex", "selected_model": "gpt-6-astra",
     "served_model": "gpt-6-sol", "tokens": 7249, "cost_usd": .25,
     "provider_usage": {"input_tokens": 100, "output_tokens": 20, "cache_in_input": True,
                        "cache_read_tokens": 5000, "cache_write_tokens": 300}},
    {"event_id": "use2", "kind": "ask", "provider": "codex", "served_model": "gpt-6-sol"},
    {"event_id": "use3", "kind": "ask", "provider": "codex", "selected_model": "gpt-6-sol", "tokens": 77,
     "provider_usage": {"input_tokens": 0, "output_tokens": 0, "cache_in_input": True}},
    {"event_id": "use4", "kind": "review", "provider": "claude", "selected_model": "claude-fable-5-1",
     "tokens": 7, "cost_usd": .5, "model_attribution": "ambiguous",
     "provider_usage": {"input_tokens": 1, "output_tokens": 7, "cache_in_input": False,
                        "cache_read_tokens": 5, "cache_write_tokens": 3}},
    {"event_id": "use5", "kind": "delegate", "provider": "claude", "selected_model": "claude-opus-5-5",
     "tokens": 9, "cost_usd": .1, "fallback_used": True,
     "provider_usage": {"input_tokens": 2, "output_tokens": 9, "cache_in_input": False,
                        "cache_read_tokens": 4, "cache_write_tokens": 3}},
    {"event_id": "owner", "kind": "owner-finalize", "tokens": 99999, "cost_usd": 100},
    {"event_id": "gate", "kind": "review-outcome", "tokens": 99999, "cost_usd": 100}]}
usage_view["operations"]["recent"].append(deepcopy(usage_view["operations"]["recent"][0]))
usage_view["attempts"]["recent"] = [{"attempt_id": "use1", "tokens": 50000, "state": "done"}]
groups = model_usage(usage_view)
assert len(groups) == 3 and sum(g["records"] for g in groups) == 5, groups
served, selected, mixed = groups
assert (served["name"], served["tokens"], served["token_reports"], served["records"]) == ("Sol 6", 120, 1, 2), groups
assert selected["attribution"] == "selected" and selected["tokens"] == 0 and selected["token_reports"] == 1
assert mixed["name"] == "Mixed models" and mixed["tokens"] == 34 and abs(mixed["cost"] - .6) < .000001
labels = " | ".join(usage_labels(usage_view, costs=True))
assert "120+? tok" in labels and "$0.25+?" in labels and "Sol 6(sel) 0 tok" in labels, labels
assert "Astra" not in labels and "Opus" not in labels and "$?" in labels, labels
assert "USAGE" not in render(usage_view, "codex", 100, 28, view="tree")
assert "USAGE" not in render(usage_view, "claude", 42, 34, main_attempt="att_main", view="tree")
assert model_usage(usage_view) == model_usage({**usage_view, "attempts": {}})
bad_usage = {"operations": {"recent": [{"kind": "ask", "tokens": True, "cost_usd": float("nan")}]}}
assert "? tok" in usage_labels(bad_usage, costs=True)[0] and "$?" in usage_labels(bad_usage, costs=True)[0]
assert model_usage(bad_usage)[0]["token_reports"] == 0
assert "Usage unavailable" in render({"operations": {"available": False}}, "codex", 42, 34)
crowded_scope = render(tree, "codex", 32, 24, main_attempt="att_main", view="detail", attention_only=True,
                       previous="ready")
overflow_row = next(line for line in crowded_scope.splitlines() if "v Expand" in line)
assert "f2" in overflow_row and "out2" in overflow_row and "..." not in overflow_row, crowded_scope

# Wide compact rows use one line per call. Tall detail retains the same titles
# and metadata; narrow cards preserve title space without tree indentation.
many_calls = deepcopy(tree)
many_calls["attempts"]["active_recent"] = [many_calls["attempts"]["active_recent"][0]]
for n in range(8):
    call_row = deepcopy(tree["attempts"]["active_recent"][2])
    call_row.update(attempt_id="many_%s" % n, task_id="many_%s" % n)
    call_row["panel"]["label"] = "Distinct task %s" % n
    many_calls["attempts"]["active_recent"].append(call_row)
many_text = render(many_calls, "codex", 100, 28)
assert all("Distinct task %s" % n in many_text for n in range(8)), many_text
assert "more" not in many_text, many_text


for width, height in ((100, 28), (42, 34), (24, 18), (12, 10), (2, 8)):
    for density in ("auto", "compact", "detail"):
        for filtered in (False, True):
            for glyphs in (False, True):
                text = render(tree, "codex", width, height, view=density, attention_only=filtered, unicode=glyphs, menu=True)
                assert len(text.splitlines()) <= height - 1 and all(display_width(line) <= width for line in text.splitlines()), text
                closed_cards(text, width, glyphs)
for width, height in ((80, 28), (90, 28), (30, 20), (42, 24), (100, 24)):
    for density in ("auto", "compact", "detail"):
        for glyphs in (False, True):
            text = render(usage_view, "codex", width, height, view=density, unicode=glyphs,
                          menu=True, managed=True, previous="ready")
            closed_cards(text, width, glyphs)
idle = deepcopy(live)
idle["attempts"]["active_recent"][0]["state"] = "blocked"
assert render(idle, "codex", 42, 34, frame=0) == render(idle, "codex", 42, 34, frame=1)
for width, height in ((100, 32), (42, 34), (24, 18), (12, 10), (2, 8)):
    for frame in (None, 0, 1):
        text = render(live, "codex", width, height, frame=frame)
        assert len(text.splitlines()) <= height and all(display_width(line) <= width for line in text.splitlines()), text
# Frames use one cached snapshot. --count still counts collections, and resize
# triggers one complete redraw rather than leaving stale lines in the pane.
import io
from unittest.mock import patch
with patch.dict(os.environ, {"OMS_CODEX_NOTIFY": "0", "OMS_CLAUDE_NOTIFY": "0"}, clear=True):
    assert "OMS_CLAUDE_NOTIFY=0" in panel.panel_environment()
for mode in ("animated", "static", "environment", "interrupt"):
    clock = [0.0]
    output = io.StringIO()
    def sleep(seconds):
        if mode == "interrupt":
            raise KeyboardInterrupt
        clock[0] += seconds
    # Keep real collector subprocesses/threads out of the simulated refresh clock.
    with patch.object(panel, "read_panel", return_value=(live, 0, "codex", None, "auto", False, "")) as collect, \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", output), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((42, 34))), \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=sleep), \
            patch.dict(os.environ, {"OMS_PANEL_NO_ANIMATION": "1" if mode == "environment" else "0"}):
        try:
            assert panel.watch(Path("."), "codex", count=2, no_animation=mode == "static") == 0
            assert mode != "interrupt", "watch swallowed the interrupt"
        except KeyboardInterrupt:
            assert mode == "interrupt"
    assert collect.call_count == (1 if mode == "interrupt" else 2)
    assert clock[0] == (0 if mode == "interrupt" else 5.0), clock
    assert output.getvalue().count("\033[2J") == 1 and "\033[?25h" in output.getvalue(), output.getvalue()
    assert ("\033[2K" in output.getvalue()) == (mode == "animated")
# A blocked state read must not block view keys or quitting the terminal.
import threading
import time
for pressed, shown in [("t", "tree")]:
  entered, released, finished_read = threading.Event(), threading.Event(), threading.Event()
  def blocked_snapshot(*args, **kwargs):
      entered.set()
      try:
          assert released.wait(3), "fixture failed to release the read"
          return live, 0
      finally:
          finished_read.set()
  with patch.object(panel, "snapshot", side_effect=blocked_snapshot), \
          patch.object(panel, "managed_session", return_value=False), \
          patch.object(panel, "terminal_style", return_value=(True, False, True)), \
          patch.object(panel.sys, "stdout", io.StringIO()), \
          patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
          patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
          patch("panel_input.TerminalInput", ResponsiveInput):
      started = time.monotonic()
      try:
          assert panel.watch(Path("."), "codex", no_animation=True) == 0
          assert time.monotonic() - started < .5, "blocked collector delayed input"
          assert display.call_args.kwargs["view"] == shown
      finally:
          released.set()
          assert finished_read.wait(1)

# Keys run against the board that was drawn: Esc closes ? help alone, and a click after a redraw in the same read is dropped.
def drive(batches, view="graph"):
    seen, kept = [], []
    def record(*args, **kwargs):
        seen.append(kwargs["navigation"])
        return panel_frame_view(*args, **kwargs)
    with patch.object(panel, "snapshot", return_value=(live, 0)), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(True, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
            patch.object(panel, "frame_view", side_effect=record), \
            patch("panel_input.choose", side_effect=lambda event, nav: kept.append(event) or choose(event, nav)), \
            patch("panel_input.TerminalInput", lambda *a, **k: BatchInput(batches)):
        assert panel.watch(Path("."), "codex", view=view, no_animation=True) == 0
    return seen[-1], kept
panel_frame_view = panel.frame_view
helped, unused = drive([[("?",)], [("escape",)]])
assert not helped.get("keys_help") and not helped.get("dismissed"), helped
healthy, routed = drive([[("k",)], [("a",), ("n",), ("repaint",), ("u",)], [("escape",)]])
assert healthy.get("health_view") is False and not healthy.get("dismissed"), healthy
assert not any(event[0] in {"k", "u", "repaint", "a", "n"} for event in routed), routed
unused, kept = drive([[("t",), ("click", 99, 39)]])
assert not any(event[0] == "click" for event in kept), kept
unused, kept = drive([[("click", 99, 39)]])
assert [event[0] for event in kept] == ["click"], kept

# A completed refresh must preserve standalone view choices made with keys.
clock = [0.0]
def change_view(seconds):
    clock[0] += 5
    return [("g",)]
with patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "read_panel", return_value=(live, 0, "codex", None, "summary", False, "")), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
        patch.object(ResponsiveInput, "wait", side_effect=change_view), \
        patch("panel_input.TerminalInput", ResponsiveInput):
    assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert display.call_args.kwargs["view"] == "graph", "refresh reset the key-selected view"

# Window controls can change between snapshots. Explicit CLI choices persist;
# rendered text must never overwrite the density option on later refreshes.
for requested, forced in ((None, False), ("compact", True)):
    clock = [0.0]
    def pane_value(name):
        return {"@oms_panel_owner": "codex", "@oms_panel_main_attempt": "att_main",
                "@oms_panel_view": "compact" if clock[0] < 5 else "detail",
                "@oms_panel_attention": "0" if clock[0] < 5 else "1"}.get(name, "")
    with patch.object(panel, "snapshot", return_value=(tree, 0)), \
            patch.object(panel, "managed_session", return_value=True), \
            patch.object(panel, "pane_option", side_effect=pane_value), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds)):
        assert panel.watch(Path("."), "codex", count=2, view=requested, attention_only=forced) == 0
    assert [c.kwargs["view"] for c in display.call_args_list] == (["compact", "compact"] if requested else ["compact", "detail"])
    assert [c.kwargs["attention_only"] for c in display.call_args_list] == ([True, True] if forced else [False, True])
# A flag-set attention filter (--attention-only, as the Herdr watcher uses) still yields to the b key.
clock = [0.0]
def press_b(seconds):
    clock[0] += 5
    return [("b",)] if clock[0] == 5 else []
with patch.object(panel, "BackgroundRead", ImmediateRead), \
        patch.object(panel, "snapshot", return_value=(live, 0)), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as display, \
        patch.object(ResponsiveInput, "wait", side_effect=press_b), \
        patch("panel_input.TerminalInput", ResponsiveInput):
    assert panel.watch(Path("."), "codex", count=3, no_animation=True, attention_only=True) == 0
    assert [c.kwargs["attention_only"] for c in display.call_args_list][0] is True
    assert [c.kwargs["attention_only"] for c in display.call_args_list][-1] is False, display.call_args_list
with patch.object(panel.sys, "stdout", io.StringIO()) as output:
    last = panel.draw_frame("first\nsecond", (42, 34), True)
    panel.draw_frame("first\nchanged", (42, 34), True, last)
    panel.draw_frame("small", (24, 18), True, last)
    assert output.getvalue().count("\033[2J") == 2 and "\033[2;1H\033[2Kchanged" in output.getvalue()
from unittest.mock import patch
with patch.object(panel.subprocess, "run", side_effect=subprocess.TimeoutExpired("tmux", 5)):
    assert panel.pane_option("@oms_panel_view") == ""
# A failed collection remains visible and the same watcher recovers on its next
# refresh. Invalid window selection must not display another room as selected.
import thread_live
import room
for failure in (([], 0), subprocess.TimeoutExpired("dashboard", 5)):
    clock = [0.0]
    responses = [failure, (live, 0)]
    with patch.object(panel, "dashboard", side_effect=responses), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds)), \
            patch.dict(os.environ, {}, clear=True), patch.object(thread_live, "current_thread", return_value=None):
        assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert displayed.call_args_list[0].args[2]["collection"]["ok"] is False
    assert displayed.call_args_list[1].args[2] == live
with patch.object(panel, "snapshot", return_value=(live, 0)), \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "pane_option", side_effect=lambda name: "invalid room" if name == "@oms_panel_room" else ""), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed:
    assert panel.watch(Path("."), "codex", count=1, no_animation=True) == 1
assert "invalid window room selection" in displayed.call_args.args[2]["room"]["error"]
# Non-TTY exceptions from a background collector follow the synchronous error path,
# including a second read after a successful frame; they cannot become held success.
class CompletedRead(panel.BackgroundRead):
    def start(self, reader):
        super().start(reader)
        result = self.ready.get(timeout=2)
        self.ready.put(result)
for successful_reads in (0, 1):
    clock = [0.0]
    with patch.object(panel, "BackgroundRead", CompletedRead), \
            patch.object(panel, "read_panel", side_effect=[(live, 0, "codex", None, "auto", False, "")] * successful_reads +
                         [ValueError("fixture collection unavailable")]), \
            patch.object(panel, "repository", return_value=Path(".")), \
            patch.object(panel, "git_toplevel", return_value=Path(".")), \
            patch.object(panel, "work_repository", return_value=Path(".")), \
            patch.object(panel, "native_repository", return_value=Path(".")), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "terminal_style", return_value=(False, False, True)), \
            patch.object(panel.sys, "stdout", io.StringIO()), \
            patch.object(panel.sys, "stderr", io.StringIO()) as errors, \
            patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
            patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
            patch.object(panel.time, "sleep", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds)), \
            patch.dict(os.environ, {}, clear=True):
        assert panel.main(["--repo", ".", "--watch", "--count", "2", "--no-animation"]) == 1
        assert errors.getvalue() == "error: fixture collection unavailable\n"
        assert displayed.call_count == successful_reads, "collector exception displayed stale success"
# Interactive collection errors remain failed frames, then recover on the next read.
clock = [0.0]
output = io.StringIO()
with patch.object(panel, "BackgroundRead", CompletedRead), \
        patch.object(panel, "read_panel", side_effect=[ValueError("fixture collection unavailable"),
                     (live, 0, "codex", None, "auto", False, "")]) as collect, \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "terminal_style", return_value=(True, False, True)), \
        patch.object(panel.sys, "stdout", output), patch.object(output, "isatty", return_value=True), \
        patch.object(panel.os, "get_terminal_size", return_value=os.terminal_size((100, 40))), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(ResponsiveInput, "wait", side_effect=lambda seconds: clock.__setitem__(0, clock[0]+seconds) or []), \
        patch("panel_input.TerminalInput", ResponsiveInput), patch.dict(os.environ, {}, clear=True):
    assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert collect.call_count == displayed.call_count == 2
    assert displayed.call_args_list[0].args[2]["collection"]["ok"] is False
    assert displayed.call_args_list[1].args[2] == live
    assert "Room state unavailable" in output.getvalue(), output.getvalue()
clock = [0.0]
with patch.object(panel, "dashboard", side_effect=[(deepcopy(live), 0), (deepcopy(live), 0)]), \
        patch.object(room, "status", side_effect=lambda repo, ident: {"id": ident}), \
        patch.object(thread_live, "current_thread", return_value="unrelated-current") as current, \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel, "pane_option", side_effect=lambda name: "selected-room" if name == "@oms_panel_room" and clock[0] < 5 else ""), \
        patch.object(panel, "terminal_style", return_value=(False, False, True)), \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "frame_view", wraps=panel.frame_view) as displayed, \
        patch.object(panel.time, "monotonic", side_effect=lambda: clock[0]), \
        patch.object(panel.time, "sleep", side_effect=lambda s: clock.__setitem__(0, clock[0]+s)), \
        patch.dict(os.environ, {}, clear=True):
    assert panel.watch(Path("."), "codex", count=2, no_animation=True) == 0
    assert displayed.call_args_list[0].args[2]["room"]["id"] == "selected-room"
    assert "room" not in displayed.call_args_list[1].args[2]
    assert current.call_count == 0
# Choose an actual result by number. Only its task ID is used for the detail
# query; returning to the list does not call or retry a model or app receiver.
sample_rows = [{"task_id": "one", "title": "First result", "summary": "FIRST DETAIL"},
               {"task_id": "two", "title": "Second result", "summary": "SECOND DETAIL"}]
def result_query(repo, task_id=None):
    return {"rows": [r for r in sample_rows if not task_id or r["task_id"] == task_id]}
with patch.object(panel, "results", side_effect=result_query) as query, \
        patch.object(panel, "read_field", side_effect=["0", "2", "", ""]), \
        patch.object(panel.sys, "stdout", io.StringIO()) as output:
    panel.browse_results(Path("."))
    assert query.call_args_list[2].args == (Path("."), "two"), query.call_args_list
    assert "SECOND DETAIL" in output.getvalue() and "FIRST DETAIL" not in output.getvalue()
# OPERATOR_FIXTURE_BEGIN control
def empty_control(*args, **kwargs):
    kwargs["cache"].update(status=0, state={"collection": {"ok": True}, "attempts": {"active_recent": []}})
    kwargs["navigation"]["main_occupied"] = False
    return 0
# OPERATOR_FIXTURE_END control
with patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "show", side_effect=empty_control) as display, \
        patch.object(panel, "read_field", side_effect=["v", "b", "u", "g", "q"]):
    assert panel.interactive(Path(".")) == 0
    assert [c.kwargs["view"] for c in display.call_args_list] == ["auto", "auto", "auto", "auto", "graph"]
    assert [c.kwargs["attention_only"] for c in display.call_args_list] == [False] * 5
for setting in ({"TERM": "dumb"}, {"TERM": "xterm", "NO_COLOR": ""}):
    saved = dict(os.environ)
    try:
        os.environ.update(setting)
        with patch.object(panel.sys.stdout, "isatty", return_value=True):
            assert panel.terminal_style()[1] is False
    finally:
        os.environ.clear()
        os.environ.update(saved)
for setting, expected in (({"TERM": "xterm", "NO_COLOR": "1", "OMS_PANEL_COLOR": "always"}, True),
                          ({"TERM": "xterm", "OMS_PANEL_COLOR": "never"}, False),
                          ({"TERM": "dumb", "OMS_PANEL_COLOR": "always"}, False)):
    with patch.dict(os.environ, setting, clear=True), patch.object(panel.sys.stdout, "isatty", return_value=True):
        assert panel.terminal_style()[1] is expected
        assert "OMS_PANEL_COLOR=" + setting["OMS_PANEL_COLOR"] in panel.panel_environment()
with patch.dict(os.environ, {"TERM": "xterm", "OMS_PANEL_COLOR": "always"}, clear=True), \
        patch.object(panel.sys.stdout, "isatty", return_value=False):
    assert panel.terminal_style()[1] is False, "redirected output must stay plain"
