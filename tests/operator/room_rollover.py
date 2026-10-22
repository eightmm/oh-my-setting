# A room past its rollover marks continues as a derived successor: running ids keep working, history stays.
import threading
roll = temporary / "rollover repo"
roll.mkdir()
subprocess.run(["git", "init", "-q", str(roll)], check=True)
with patch.dict(os.environ, {}):
    for name in [n for n in os.environ if n.startswith(("OMS_ROOM_", "OMS_PANEL_")) or n in (
            "OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH", "TMUX", "TMUX_PANE")]:
        del os.environ[name]
    old = room.create(roll, title="All-day room")
    lead = "main-roll"
    room.join(roll, old, lead, "claude", "main", native_session="roll-session", owns=["docs"])
    room.describe(roll, old, lead, "Rolling main")
    for call_name in ("call-run", "call-pend", "call-done"):
        room.join(roll, old, call_name, "codex", "worker", parent=lead, owns=["src/" + call_name])
    assigned = {c: room.send(roll, old, lead, c, "Assigned: " + c, "handoff")["id"]
                for c in ("call-run", "call-pend", "call-done")}
    question = room.send(roll, old, lead, "call-run", "Which file?", "question")["id"]
    room.send(roll, old, "call-pend", lead, "Call exit=0; pending read", "handoff", message_id="result-call-pend")
    room.send(roll, old, "call-done", lead, "Call exit=1; read", "handoff", message_id="result-call-done")
    fyi = room.send(roll, old, "call-run", lead, "FYI before rollover", "note")["id"]
    settled = room.send(roll, old, "call-run", lead, "Read before rollover", "note")["id"]
    room.acknowledge(roll, old, lead, ["result-call-done", settled])
    room.send(roll, old, lead, "all", "first status", "status")
    latest_status = room.send(roll, old, lead, "all", "latest status", "status")["id"]
    old_cursor = room.updates(roll, old, lead)["cursor"]
    # Bulk settled mail to the finished call, written as valid rows instead of 900 subprocess appends.
    log = roll / ".oms/threads" / (old + ".jsonl")
    rows = room.records(roll, old)
    stamp = rows[-1]["ts"]
    with log.open("a", encoding="utf-8") as handle:
        for n in range(room.ROLLOVER_MESSAGES + 1 - len(room.project(rows)["messages"])):
            handle.write(json.dumps({"schema": 1, "ts": stamp, "thread": old, "seq": len(rows) + n + 1, "role": "note",
                                     "agent": "agent", "text": "bulk %d" % n,
                                     "room_event": {"kind": "message", "id": "bulk-%d" % n, "sender": lead,
                                                    "recipient": "call-done", "message_kind": "note"}}) + "\n")
    full = room.project(room.records(roll, old))
    assert len(full["messages"]) == room.ROLLOVER_MESSAGES + 1
    old_bytes = log.read_bytes()
    new = room.successor_id(old)
    assert re.fullmatch(r"room-[0-9a-f]{12}", new) and room.resolve(roll, old) == old
    # Unread mail beyond the message reserve stays in the sealed log, newest first; essential state always travels.
    with patch.object(room, "MESSAGE_RESERVE", 0):
        writes, dropped = room.carry_plan(full)
    assert [m["id"] for m in dropped] == [fyi] and {"result-call-pend", question} <= {
        e["id"] for e, _, _ in writes if e["kind"] == "message"}
    # Essential carry beyond its bound is refused before any write: the old log and the successor id stay untouched.
    with patch.object(room, "CARRY_MESSAGES", 1):
        try:
            room.rollover(roll, old)
        except ValueError as error:
            assert "carry exceeds reserve" in str(error), error
        else:
            raise AssertionError("an oversized carry rolled the room over")
    assert log.read_bytes() == old_bytes and not (roll / ".oms/threads" / (new + ".jsonl")).exists()
    # Crash after the first carry, then after the seal; reruns converge on one successor without duplicates.
    real_carry, calls, late = room.carry, [], {}
    def crashing(stage):
        def carry(repo, state, target, bounded=True):
            calls.append(stage)
            real_carry(repo, state, target, bounded)
            if stage == "pre" and len(calls) == 1:
                raise OSError("fixture crash after carry")
            if stage == "post" and calls.count("post") == 2:
                raise OSError("fixture crash after seal")
        return carry
    with patch.object(room, "carry", side_effect=crashing("pre")):
        try:
            room.rollover(roll, old, lead)
        except OSError:
            pass
    assert room.resolve(roll, old) == old and room.records(roll, new)[1]["room_event"]["predecessor"] == old
    # Until the seal lands the old room stays the native session's binding, though its successor holds the carry.
    assert room.selected(roll, room.participant(full, lead)["consumer"]) == (old, lead)
    def concurrent(repo, state, target, bounded=True):
        calls.append("post")
        if calls.count("post") == 1:
            # A dispatch joins and assigns between the first read and the seal: the delta carry keeps it.
            room.join(roll, old, "call-late", "codex", "worker", parent=lead)
            late["assigned"] = room.send(roll, old, lead, "call-late", "Assigned: late", "handoff")["id"]
            room.acknowledge(roll, old, lead, [fyi])
        elif calls.count("post") == 2:
            # The seal landed but the delta carry never ran: the first writer it missed finishes it, then lands.
            late["writer"] = threading.Thread(target=lambda: late.update(note=room.send(
                roll, old, "call-late", lead, "late note", "note")["id"]))
            late["writer"].start()
            raise OSError("fixture crash after seal")
        return real_carry(repo, state, target, bounded)
    with patch.object(room, "carry", side_effect=concurrent):
        try:
            room.rollover(roll, old, lead)
        except OSError:
            pass
    assert room.records(roll, old)[-1]["room_event"] == {"kind": "successor", "participant": lead, "target": new}
    late["writer"].join(timeout=60)
    healed = room.status(roll, new)
    assert room.participant(healed, "call-late")["joined"] and late["note"] in {m["id"] for m in room.messages(roll, new)}
    assert lead in next(m for m in room.messages(roll, new) if m["id"] == fyi)["received_by"]
    after = room.records(roll, new)
    assert room.rollover(roll, old, lead) == new and room.records(roll, new) == after, "third rollover was not a no-op"
    events_new = [r["room_event"] for r in after if r.get("room_event")]
    ids = [e["id"] for e in events_new if e["kind"] == "message"]
    joined = [e["participant"] for e in events_new if e["kind"] == "join"]
    assert len(ids) == len(set(ids)) and len(joined) == len(set(joined)), (ids, joined)
    assert events_new[0] == {"kind": "created", "predecessor": old}, events_new[0]
    sealed = room.records(roll, old)
    assert sealed[-1]["room_event"]["kind"] == "successor" and sealed[-1]["text"] == "Room continues as " + new
    # Carried: joined main (consumer, scope, label), running and unread-result calls, mail still owed, open question
    # and the newest status. Not carried: settled mail, the finished call, and Assigned to a call that already answered.
    state = room.status(roll, new)
    members = {p["participant"]: p for p in state["participants"]}
    before = {p["participant"]: p for p in full["participants"]}
    assert members[lead]["consumer"] == before[lead]["consumer"] and members[lead]["owns"] == ["docs"]
    assert members[lead]["label"] == members[lead]["label_override"] == "Rolling main" and "call-done" not in members
    assert {"call-run", "call-pend", "call-late"} <= set(members), members
    carried = set(ids)
    assert {assigned["call-run"], question, "result-call-pend", latest_status, late["assigned"], late["note"], fyi} <= carried, carried
    assert not carried & {assigned["call-pend"], assigned["call-done"], "result-call-done", "bulk-0", settled}, carried
    assert state["statuses"][lead]["text"] == "latest status"
    assert [q["id"] for q in state["open_questions"]] == [question]
    assert state["predecessor"] == old and len(state["messages"]) < room.MESSAGE_RESERVE
    # The old log stays a readable, unclosed room: the running main is still enrolled there.
    history = room.status(roll, old, exact=True)
    assert history["successor"] == new and not history["closed"] and history["message_count"] == len(full["messages"]) + 1
    assert room.participant(history, lead)["joined"]
    shown = subprocess.run(["bash", str(panel.ENTRY), "room", "show", "--repo", str(roll), "--id", old, "--exact"],
                           capture_output=True, text=True, timeout=60)
    assert shown.returncode == 0 and json.loads(shown.stdout)["id"] == old, shown.stderr
    shown = subprocess.run(["bash", str(panel.ENTRY), "room", "show", "--repo", str(roll), "--id", old],
                           capture_output=True, text=True, timeout=60)
    assert shown.returncode == 0 and json.loads(shown.stdout)["id"] == new, shown.stderr
    # Ids naming the old room forward, even through a stale cached answer; a raw write to the sealed log is refused.
    room._RECENT[(str(roll), old)] = (room._CLOCK(), old)
    sent = room.send(roll, old, lead, "call-run", "After rollover", "note")
    assert room.messages(roll, new)[-1]["id"] == sent["id"]
    raw = subprocess.run(["bash", str(panel.ENTRY), "thread", "append", "--repo", str(roll), "--id", old,
                          "--role", "note", "--text", "bypass"], capture_output=True, text=True, timeout=60)
    assert raw.returncode and "room continues as " + new in raw.stderr, raw
    stale = room.updates(roll, old, lead, old_cursor)
    assert stale["thread"] == new and "result-call-pend" in [t["room_event"]["id"] for t in stale["turns"]], stale
    # A worker launched before the rollover keeps OMS_ROOM_ID=OLD: its ack and message land in the successor.
    worker = {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": old, "OMS_ROOM_PARTICIPANT": "call-run", "OMS_ROOM_REPO": str(roll)}
    with patch.dict(os.environ, worker):
        room.acknowledge(roll, old, "call-run", [assigned["call-run"]])
        room.send(roll, old, "call-run", lead, "Worker note", "note")
        assert "call-run" in next(m for m in room.messages(roll, new) if m["id"] == assigned["call-run"])["received_by"]
        assert room.messages(roll, new)[-1]["text"] == "Worker note"
        try:
            room.rollover(roll, new)
        except ValueError as error:
            assert "worker cannot roll" in str(error)
        else:
            raise AssertionError("a worker rolled a room over")
    # The panel's result handoff named the old room at launch and still reaches the main.
    with patch.object(panel, "_settle_dispatch", return_value=None), \
            patch.object(panel, "results", return_value={"rows": []}):
        panel.dispatch_store.initialize(roll, "call-run", {"room": old, "parent": lead, "task_id": "roll-task"})
        panel.deliver_result(roll, old, "call-run", lead, "roll-task", 0)
    assert room.messages(roll, new)[-1]["id"] == "result-call-run"
    other = room.create(roll, title="Elsewhere")
    room.join(roll, other, "main-other", "codex", "main")
    room.join(roll, other, "call-other", "codex", "worker", parent="main-other")
    with patch.dict(os.environ, dict(worker, OMS_ROOM_ID=other, OMS_ROOM_PARTICIPANT="call-other")):
        try:
            room.send(roll, old, "call-other", lead, "Cross-room note", "note")
        except ValueError as error:
            assert "admitted room participant" in str(error), error
        else:
            raise AssertionError("another room's worker wrote through a forwarded id")
    # Cancelling a running call checks room lineage: launch-time refs to the old id pass, another room's do not.
    main_attempt, call_attempt = "att_" + "a" * 32, "att_" + "b" * 32
    main_row = {"tool": "panel-main", "terminal": False, "refs": {"panel_room_id": old, "panel_room_participant": lead}}
    call_row = {"attempt_id": call_attempt, "terminal": False, "state": "working", "parent_attempt_id": main_attempt,
                "refs": {"panel_room_id": old, "panel_room_participant": "call-run"}}
    cancel_state = {"room": {"id": new}, "room_attempts": {"call-run": call_row}}
    cancel_nav = {"selected": ("result", "call-run"), "cancel_owner": {"participant": lead, "attempt": main_attempt}}
    with patch.object(panel, "_cancel_watch_authority", return_value=True), \
            patch.object(panel, "events", side_effect=lambda _repo, *args, **_kw: json.dumps(
                main_row if args[2] == main_attempt else call_row)), \
            patch.object(panel, "_dispatch_call_record", return_value={"parent": lead, "backend": "subreaper"}), \
            patch.object(panel, "_dispatch_supervisor_snapshot", return_value=(
                {"all_reaped": False, "exit": None}, {"pid": 1, "identity": "fixture", "group": 1})):
        assert panel.cancel_selected_call(roll, cancel_state, cancel_nav, main_attempt) == "ready"
        for row in (main_row, call_row):
            row["refs"]["panel_room_id"] = other
            assert panel.cancel_selected_call(roll, cancel_state, cancel_nav, main_attempt) != "ready", row
            row["refs"]["panel_room_id"] = old
    # The old env main acks mail that was settled before the rollover: a quiet no-op, not an error.
    count = len(room.records(roll, new))
    with patch.dict(os.environ, {"OMS_ROOM_ID": old, "OMS_ROOM_PARTICIPANT": lead, "OMS_ROOM_REPO": str(roll)}):
        room.acknowledge(roll, old, lead, ["result-call-done"])
        assert len(room.records(roll, new)) == count
        # A reply to mail settled before the rollover is still sent, unlinked from the message left behind.
        reply = room.send(roll, old, lead, "call-run", "Re: read before rollover", "note", reply_to=settled)
        assert "reply_to" not in reply and room.messages(roll, new)[-1]["id"] == reply["id"]
        room.describe(roll, old, lead, "Rolling main, day two")
    assert room.participant(room.status(roll, old), lead)["label"] == "Rolling main, day two"
    # Scope evidence of a finished worker is read from the room it joined, where the successor has no trace of it.
    import panel_results
    assert panel_results._worker_owns(roll, {"refs": {"panel_room_id": old, "panel_room_participant": "call-done"}}) == [
        "src/call-done"]
    # A successor row whose target does not name the room back is not followed; writes meet the seal.
    forged = room.create(roll, title="Forged")
    room.join(roll, forged, "main-forged", "codex", "main")
    rows = room.records(roll, forged)
    with (roll / ".oms/threads" / (forged + ".jsonl")).open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"schema": 1, "ts": stamp, "thread": forged, "seq": len(rows) + 1, "role": "note",
                                 "agent": "agent", "text": "Room continues as " + new,
                                 "room_event": {"kind": "successor", "participant": "main-forged", "target": new}}) + "\n")
    assert room.resolve(roll, forged) == forged
    try:
        room.send(roll, forged, "main-forged", "all", "Into the forged room", "status")
    except ValueError as error:
        assert "room continues as" in str(error), error
    else:
        raise AssertionError("a write passed a forged seal")
    try:
        room._write(roll, other, {"kind": "successor", "participant": "main-other", "target": new}, "forged")
    except ValueError as error:
        assert "does not derive" in str(error), error
    else:
        raise AssertionError("a successor row named a room that does not derive from it")
    # Discovery reads a sealed log's header and tail only; it neither fills the room bound nor the byte budget.
    states, incomplete = room.discover(roll)
    stub = next(s for s in states if s["id"] == old)
    assert stub["sealed"] and stub["successor"] == new and not stub["participants"] and not incomplete
    live_rooms = sum(not s.get("sealed") for s in states)
    with patch.object(room, "MAX_ROOMS", live_rooms), \
            patch.object(room, "MAX_SCAN_BYTES", sum(len(p.read_bytes()) for p in (roll / ".oms/threads").glob("*.jsonl")
                                                    if p.stem != old) + 4096):
        states, incomplete = room.discover(roll)
        assert not incomplete and {s["id"] for s in states} >= {old, new, other}
    assert "%s -> %s / " % (old, new) in "\n".join(panel.room_listing(states, new, 200))
    assert room.selected(roll, members[lead]["consumer"]) == (new, lead)
    # The board's Past tab lists the previous room's settled calls; the next dispatch is admitted again.
    assert [c["participant"] for c in room.predecessor_calls(roll, room.status(roll, new))] == ["call-done"]
    import room_view
    past_navigation = {"tab": "past"}
    successor_view = room.status(roll, new)
    room_view.tab_box({"room": dict(successor_view, predecessor_calls=room.predecessor_calls(roll, successor_view))},
                      100, 12, past_navigation)
    assert ("result", "call-done") in past_navigation["past_ids"], past_navigation
    # Attempts launched before the rollover name the old room and still draw on the successor's board.
    drawn = room_view.nodes({"room": successor_view, "attempts": {"active_recent": [{
        "attempt_id": "att-roll", "state": "working",
        "panel": {"room_id": old, "room_participant": lead, "role": "main", "label": "Launch label"}}]}})
    assert next(n for n in drawn if n["participant"] == lead)["attempt"] == "att-roll", drawn
    filled = room.project(room.records(roll, new))
    panel._dispatch_capacity(new, filled)
    assert panel._rollover_if_needed(roll, new, filled, lead) is None
    # A rollover cut short after its seal still moves the dispatch on; rerunning it finishes the carry.
    def cut(repo, state, target, bounded=True):
        calls.append("third")
        if calls.count("third") > 1:
            raise subprocess.TimeoutExpired("thread append", 15)
        moved = real_carry(repo, state, target, bounded)
        room.join(roll, new, "call-gap", "codex", "worker", parent=lead)
        return moved
    errors = io.StringIO()
    with patch.object(room, "ROLLOVER_MESSAGES", 1), patch.object(room, "carry", side_effect=cut), redirect_stderr(errors):
        third = panel._rollover_if_needed(roll, new, filled, lead)
    assert third == room.successor_id(new) and room.resolve(roll, old) == third
    assert "could not roll over" in errors.getvalue() and "call-gap" not in {
        p["participant"] for p in room.status(roll, third)["participants"]}, errors.getvalue()
    assert room.rollover(roll, new, lead) == third and room.participant(room.status(roll, third), "call-gap")["joined"]
    panel._dispatch_capacity(third, room.project(room.records(roll, third)))
    # The CLI rolls a room by hand; naming an already rolled id returns the room it continues as.
    for named, expected in ((other, room.successor_id(other)), (old, third)):
        by_hand = subprocess.run(["bash", str(panel.ENTRY), "room", "rollover", "--repo", str(roll), "--id", named],
                                 capture_output=True, text=True, timeout=120)
        assert by_hand.returncode == 0 and json.loads(by_hand.stdout) == {"id": expected, "rolled": named}, by_hand
    assert room.resolve(roll, other, fresh=True) == room.successor_id(other)
    # A successor id already taken by an unrelated room is refused, never adopted.
    squat = room.create(roll, title="Squatted")
    room.join(roll, squat, "main-squat", "codex", "main")
    room.create(roll, room.successor_id(squat), "Unrelated room")
    try:
        room.rollover(roll, squat)
    except ValueError as error:
        assert "successor room id collision" in str(error), error
    else:
        raise AssertionError("a rollover adopted an unrelated room")
    # A room at the limit rolls over on the write that hit it, so a call's result handoff still reaches its main;
    # unread mail beyond the reserve stays in the sealed log and the successor says where.
    full_room = room.create(roll, title="Full room")
    room.join(roll, full_room, "main-full", "codex", "main")
    legacy_main_join(roll, full_room, "main-full-b", "codex", "main")
    room.join(roll, full_room, "call-full", "codex", "worker", parent="main-full")
    unread = room.send(roll, full_room, "main-full-b", "main-full", "Old unread note", "note")["id"]
    rows = room.records(roll, full_room)
    with (roll / ".oms/threads" / (full_room + ".jsonl")).open("a", encoding="utf-8") as handle:
        for n in range(room.MAX_MESSAGES - 1):
            handle.write(json.dumps({"schema": 1, "ts": stamp, "thread": full_room, "seq": len(rows) + n + 1,
                                     "role": "note", "agent": "agent", "text": "status %d" % n,
                                     "room_event": {"kind": "message", "id": "status-%d" % n, "sender": "main-full",
                                                    "recipient": "all", "message_kind": "status"}}) + "\n")
    with patch.object(room, "MESSAGE_RESERVE", 0), \
            patch.object(panel, "_settle_dispatch", return_value=None), \
            patch.object(panel, "results", return_value={"rows": []}):
        panel.dispatch_store.initialize(roll, "call-full", {"room": full_room, "parent": "main-full", "task_id": "full-task"})
        panel.deliver_result(roll, full_room, "call-full", "main-full", "full-task", 0)
    landed = [m["id"] for m in room.messages(roll, room.successor_id(full_room))]
    assert landed[-1] == "result-call-full" and unread not in landed, landed
    assert [m for m in landed if m.startswith("rollover-unread-")], landed
    # The rollover holds in the environment a panel main really runs in (terminal_panel exports its room,
    # participant, repo and attempt), not only in a stripped one: each main below acts under its own binding.
    def rollover_regressions(where, panel_env):
        subprocess.run(["git", "init", "-q", str(where)], check=True)

        def bound(ident, who):
            return patch.dict(os.environ, {"OMS_ROOM_ID": ident, "OMS_ROOM_PARTICIPANT": who,
                                           "OMS_ROOM_REPO": str(where), "OMS_PANEL_MAIN_ATTEMPT": "att-" + who}
                              if panel_env else {})

        def fresh(*mains, calls=()):
            ident = room.create(where, title="Regression room")
            for who in mains:
                legacy_main_join(where, ident, who, "claude" if who.endswith("1") else "codex", "main",
                          native_session="sess-" + ident + "-" + who)
            for who, parent in calls:
                room.join(where, ident, who, "codex", "worker", parent=parent)
            return ident

        def fill(ident, sender, count):
            rows = room.records(where, ident)
            with (where / ".oms/threads" / (ident + ".jsonl")).open("a", encoding="utf-8") as handle:
                for n in range(count):
                    handle.write(json.dumps({"schema": 1, "ts": rows[-1]["ts"], "thread": ident, "seq": len(rows) + n + 1,
                                             "role": "note", "agent": "agent", "text": "status %d" % n,
                                             "room_event": {"kind": "message", "id": "fill-%s-%d" % (ident, n),
                                                            "sender": sender, "recipient": "all",
                                                            "message_kind": "status"}}) + "\n")

        def member(ident, who):
            return next(p for p in room.status(where, ident)["participants"] if p["participant"] == who)

        # Structured evidence stays in sealed history; quiet carried results never wake the main.
        reviewed = fresh("m1", calls=(("call-reviewed", "m1"),))
        result_event = {"kind": "call-result", "event_id": "call-result-rollover", "task_id": "rollover-review",
                        "call_id": "call-reviewed", "attempt_id": "attempt-reviewed", "role": "worker",
                        "access": "write", "exit": 0, "verify_status": "passed", "verify_exit": 0,
                        "patch_sha256": "a" * 64, "changed_files": ["scripts/fixture.py"], "needs_review": True}
        def review_claim(attempt):
            claim_id, failed_id, reviewer = room.review_ids("call-reviewed", "a" * 64, attempt)
            claim = {"kind": "review-dispatch", "event_id": claim_id, "task_id": "rollover-review",
                     "call_id": "call-reviewed", "patch_sha256": "a" * 64, "review_call_id": reviewer,
                     "attempt": attempt}
            return claim, dict(claim, kind="review-dispatch-failed", event_id=failed_id)
        claim, failure = review_claim(1)
        with bound(reviewed, "m1"):
            room.append(where, reviewed, result_event, "Structured call result")
            assert room.claim_review(where, reviewed, claim)
            # Unread direct mail keeps this finished call in the carry, including its quiet result.
            kept = room.send(where, reviewed, "call-reviewed", "m1", "Review observation")["id"]
            room.send(where, reviewed, "call-reviewed", "m1", "Call exit=0; parent acceptance pending.",
                      "handoff", message_id="result-call-reviewed", quiet=True)
            moved = room.rollover(where, reviewed, "m1")
            carried_result = next(m for m in room.messages(where, moved) if m["id"] == "result-call-reviewed")
            assert carried_result.get("quiet") is True and carried_result["pending_for"] == []
            assert not room.visible(next(row for row in room.records(where, moved)
                                         if row.get("room_event", {}).get("id") == "result-call-reviewed"), "m1")
            assert not any(row.get("room_event", {}).get("id") == "result-call-reviewed"
                           for row in room.updates(where, moved, "m1")["turns"])
            assert result_event in room.structured_events(where, moved)
            before_result = room.records(where, moved)
            room.append(where, reviewed, result_event, "Structured call result")
            assert room.records(where, moved) == before_result, "rollover duplicated a structured result"
            assert room.status(where, moved)["call_results"]["call-reviewed"]["patch_sha256"] == "a" * 64
            assert panel._review_dispatch_context(where, moved, claim["review_call_id"]) == claim
            assert not room.claim_review(where, moved, claim)
            retry_claim, retry_failure = review_claim(2)
            assert not room.claim_review(where, moved, retry_claim), "rollover reset an active review digest"
            room.fail_review(where, reviewed, failure)
            room.acknowledge(where, moved, "m1", [kept])
            next_room = room.rollover(where, moved, "m1")
            assert "call-reviewed" not in {p["participant"] for p in room.status(where, next_room)["participants"]}
            assert room.claim_review(where, next_room, retry_claim), "failed review lost its bounded retry"
            room.fail_review(where, reviewed, retry_failure)
            last_room = room.rollover(where, next_room, "m1")
            assert not room.claim_review(where, last_room, review_claim(3)[0])
            assert room.structured_events(where, reviewed).count(result_event) == 1
            with patch.object(room, "MAX_HOPS", 2):
                try:
                    room.structured_events(where, last_room)
                except ValueError as error:
                    assert "lineage is too long" in str(error), error
                else:
                    raise AssertionError("truncated lineage reset structured review history")
            assert room.status(where, last_room)["call_results"]["call-reviewed"]["attempt_id"] == "attempt-reviewed"
            past = room.predecessor_calls(where, room.status(where, last_room))
            assert [call["participant"] for call in past] == ["call-reviewed"] and past[0]["exit"] == 0, past
            # The original attempt binding still resolves when a late result is built in the successor.
            attempt = {"attempt_id": "attempt-reviewed", "task_id": "rollover-review",
                       "refs": {"panel_room_id": reviewed, "panel_room_participant": "call-reviewed",
                                "panel_role": "worker", "panel_access": "write"}}
            with patch.object(panel_result_store, "_records", return_value=([attempt], [])):
                late_result, _ = panel._call_result_event(where, last_room, "call-reviewed", "rollover-review", 0)
                assert late_result["attempt_id"] == "attempt-reviewed" and late_result["access"] == "write"
        for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
            with patch.dict(os.environ, dict(guard, OMS_ROOM_ID=reviewed, OMS_ROOM_REPO=str(where),
                                            OMS_ROOM_PARTICIPANT="call-reviewed")):
                for target in (reviewed, moved, next_room, last_room):
                    before = room.records(where, last_room)
                    for event in (result_event, claim, failure):
                        try:
                            room.append(where, target, event, "Worker structured forgery")
                        except ValueError as error:
                            assert "worker can only message or acknowledge" in str(error), error
                        else:
                            raise AssertionError("worker wrote structured lineage evidence")
                    assert room.records(where, last_room) == before
                with patch.dict(os.environ, {"OMS_ROOM_CARRY": "1"}):
                    try:
                        room.validate_event(room.records(where, last_room), result_event, "Forged carry", repo=where)
                    except ValueError as error:
                        assert "worker cannot carry a room" in str(error), error
                    else:
                        raise AssertionError("worker carried structured evidence")

        real_carry = room.carry
        # Both described mains carry their labels, by hand, through a dispatch and through a full room's send.
        described = fresh("m1", "m2")
        for who in ("m1", "m2"):
            with bound(described, who):
                room.describe(where, described, who, "Work of " + who)
        with bound(described, "m1"):
            rolled = room.rollover(where, described, "m1")
            assert room.rollover(where, described, "m1") == rolled
            assert {p["participant"]: p.get("label_override") for p in room.status(where, rolled)["participants"]} == {
                "m1": "Work of m1", "m2": "Work of m2"}
            fill(rolled, "m1", room.ROLLOVER_MESSAGES + 1)
            errors = io.StringIO()
            with redirect_stderr(errors):
                dispatched = panel._rollover_if_needed(where, rolled, room.project(room.records(where, rolled)), "m1")
            assert dispatched == room.successor_id(rolled) and "could not" not in errors.getvalue(), errors.getvalue()
            fill(dispatched, "m1", room.MAX_MESSAGES)
            room.send(where, dispatched, "m1", "m2", "Past the limit", "handoff")
            assert room.messages(where, room.successor_id(dispatched))[-1]["text"] == "Past the limit"
            assert member(described, "m2")["label_override"] == "Work of m2"
        # Writes that land between the first carry and the seal (a leave, a rescope, a second description, a
        # native rebind, a call's result and an answer) all reach the successor; a crash after the seal is
        # finished by the next write, even from a writer already enrolled there.
        raced = fresh("m1", "m2", "m3", calls=[("call-x", "m1")])
        consumer, freed = member(raced, "m1")["consumer"], member(raced, "m3")["consumer"]
        question = room.send(where, raced, "m1", "m2", "Which file?", "question")["id"]
        broadcast = room.send(where, raced, "m1", "all", "Before the call joined", "note")["id"]
        room.join(where, raced, "call-y", "codex", "worker", parent="m1")
        legacy_main_join(where, raced, "pi1", "pi", "main", native_session="pi-one")
        steps = []
        def racing(repo, state, target, bounded=True):
            steps.append(target)
            if len(steps) == 1:
                moved = real_carry(repo, state, target, bounded)
                with bound(raced, "m3"):
                    room.append(where, raced, {"kind": "leave", "participant": "m3"}, "Participant left")
                # A session freed by a member that left is taken by a new one; a Pi main rejoins on a new session.
                legacy_main_join(where, raced, "m4", "claude", "main", native_session="sess-%s-m3" % raced)
                with bound(raced, "pi1"):
                    room.append(where, raced, {"kind": "leave", "participant": "pi1"}, "Participant left")
                legacy_main_join(where, raced, "pi1", "pi", "main", native_session="pi-two")
                with bound(raced, "m2"):
                    room.scope(where, raced, "m2", ["src"])
                    room.send(where, raced, "m2", "m1", "src.py", "answer", reply_to=question)
                    room.acknowledge(where, raced, "m2", [question])
                with bound(raced, "m1"):
                    room.describe(where, raced, "m1", "Second work")
                    room.bind(where, raced, "m1", "sess-two", replaces=consumer)
                    room.acknowledge(where, raced, "m1", [m["id"] for m in room.messages(where, raced)
                                                          if m["sender"] == "m2"])
                room.send(where, raced, "call-x", "m1", "Call exit=0; done", "handoff", message_id="result-call-x")
                return moved
            raise subprocess.TimeoutExpired("thread append", 15)
        with bound(raced, "m1"), patch.object(room, "carry", side_effect=racing):
            try:
                room.rollover(where, raced, "m1")
            except subprocess.TimeoutExpired:
                pass
        after = room.successor_id(raced)
        assert room.resolve(where, raced, fresh=True) == after and not room.status(where, after)["carried"]
        # Workers cannot carry, so a worker's write waits for a main's to finish the carry.
        with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": raced, "OMS_ROOM_PARTICIPANT": "call-y",
                                     "OMS_ROOM_REPO": str(where)}):
            try:
                room.send(where, raced, "call-y", "m1", "Before the carry", "question")
            except ValueError as error:
                assert "room carry is unfinished" in str(error), error
            else:
                raise AssertionError("a worker wrote into a half-carried successor")
        with bound(raced, "m1"):
            room.send(where, after, "m1", "m2", "Enrolled writer", "note")
        healed = room.status(where, after)
        assert healed["carried"] and healed["call_results"]["call-x"]["exit"] == 0, healed["call_results"]
        people = {p["participant"]: p for p in healed["participants"]}
        assert not people["m3"]["joined"] and people["m2"]["owns"] == ["src"], people
        assert people["m4"]["joined"] and people["m4"]["consumer"] == freed, people["m4"]
        assert people["pi1"]["consumer"] == hashlib.sha256(b"pi-two").hexdigest()[:32], people["pi1"]
        assert people["m1"]["label_override"] == "Second work" and people["m1"]["consumer"] != consumer
        assert people["m1"]["initial_consumer"] == consumer, people["m1"]
        assert room.selected(where, people["m1"]["consumer"]) == (after, "m1")
        # The answered question stays answered, and the broadcast reaches only who it reached when sent.
        assert not [q for q in healed["open_questions"] if q["id"] == question], healed["open_questions"]
        assert next(m for m in room.messages(where, after) if m["id"] == broadcast)["targets"] == ["m2", "m3", "call-x"]
        # A participant that left the successor stays gone when a later writer or a rerun meets the rollover.
        room.append(where, after, {"kind": "leave", "participant": "m2"}, "Participant left")
        try:
            room.send(where, raced, "m1", "m-typo", "Misaddressed", "note")
        except ValueError as error:
            assert "has not joined" in str(error), error
        else:
            raise AssertionError("a misaddressed note was delivered")
        with bound(raced, "m1"):
            room.rollover(where, raced, "m1")
        assert not member(after, "m2")["joined"]
        # Carried mail keeps its send time, so reminder and escalation clocks keep counting.
        aged = fresh("m1", "m2")
        asked = room.send(where, aged, "m1", "m2", "Still open?", "question")["id"]
        log = where / ".oms/threads" / (aged + ".jsonl")
        rows = [json.loads(line) for line in log.read_text().splitlines()]
        old_stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() - 1500))
        log.write_text("".join(json.dumps(dict(r, ts=old_stamp)) + "\n" for r in rows))
        with bound(aged, "m1"):
            moved = room.rollover(where, aged, "m1")
        assert next(q for q in room.open_questions(room.status(where, moved)) if q["id"] == asked)["age"] >= 1500
        # A call whose parent left is not carried; an empty successor file left by a cut-short create is replaced.
        orphaned = fresh("m1", "m2", calls=[("call-o", "m2")])
        room.append(where, orphaned, {"kind": "leave", "participant": "m2"}, "Participant left")
        (where / ".oms/threads" / (room.successor_id(orphaned) + ".jsonl")).write_bytes(b"")
        with bound(orphaned, "m1"):
            moved = room.rollover(where, orphaned, "m1")
        assert "call-o" not in {p["participant"] for p in room.status(where, moved)["participants"]}
        # A rollover cut short before its seal and rerun by another main writes its unread-mail note once; a
        # dropped message is acknowledged in the successor, and the sealed log shows it read.
        noted = fresh("m1", "m2", "m3")
        unread = [room.send(where, noted, "m3", "m1", "Unread %d" % n, "note")["id"] for n in range(3)]
        real_write = room._write
        def unsealed(repo, ident, event, text, carried=None):
            if event["kind"] == "successor":
                raise subprocess.TimeoutExpired("thread append", 15)
            return real_write(repo, ident, event, text, carried)
        with bound(noted, "m2"), patch.object(room, "_write", side_effect=unsealed), \
                patch.object(room, "MESSAGE_RESERVE", 1):
            try:
                room.rollover(where, noted, "m2")
            except subprocess.TimeoutExpired:
                pass
        room.append(where, noted, {"kind": "leave", "participant": "m2"}, "Participant left")
        legacy_main_join(where, noted, "m4", "claude", "main", native_session="sess-%s-m2" % noted)
        assert room.successor_id(noted) not in {s["id"] for s in room.discover(where)[0]}
        with bound(noted, "m1"), patch.object(room, "MESSAGE_RESERVE", 1):
            moved = room.rollover(where, noted, "m1")
            assert room.rollover(where, noted, "m1") == moved
            landed = [m for m in room.messages(where, moved) if m["id"].startswith("rollover-unread-")]
            assert len(landed) == 1 and landed[0]["sender"] == "m1", landed
            room.acknowledge(where, noted, "m1", [unread[0]])
            for wrong in (["msg-typo"], [unread[1], "msg-typo"]):
                try:
                    room.acknowledge(where, noted, "m1", wrong)
                except ValueError as error:
                    assert "another participant" in str(error), error
                else:
                    raise AssertionError("an unknown message id was acknowledged")
            # Unknown ids read each sealed log once, not once per id, both before the write and under its lock.
            reads, real_records = [], room.records
            bogus = ["bogus-%d" % n for n in range(50)]
            with patch.object(room, "records", side_effect=lambda repo, ident: reads.append(ident)
                              or real_records(repo, ident)):
                for check in (lambda: room.acknowledge(where, noted, "m1", bogus),
                              lambda: room.validate_event(room.records(where, moved), {
                                  "kind": "received", "participant": "m1", "messages": bogus}, "", repo=where)):
                    try:
                        check()
                    except ValueError as error:
                        assert "another participant" in str(error), error
                    else:
                        raise AssertionError("an unknown message id was acknowledged")
            assert reads.count(noted) <= 2, reads.count(noted)
        freed = hashlib.sha256(("sess-%s-m2" % noted).encode()).hexdigest()[:32]
        assert member(moved, "m4")["consumer"] == freed and not member(moved, "m2")["joined"]
        sealed = room.status(where, noted, exact=True)
        assert [m["id"] for m in sealed["messages"] if m["pending_for"]] == unread[1:], sealed["messages"]
        # A close racing a rollover waits for its seal and closes the successor; no open half-carried room remains.
        # It runs in its own process, which has not seen the carry complete, and keeps its stale resolve.
        closing = fresh("m1", calls=[("call-c", "m1")])
        racer = {}
        close_ready = where / "racing-close-ready"
        closer = ("import sys, time; from pathlib import Path; sys.path.insert(0, sys.argv[1]); import room; room.RESOLVE_TTL = 600\n"
                  "lock = room._rollover_lock\n"
                  "def ready_lock(*args, **kwargs):\n"
                  "    Path(sys.argv[4]).touch()\n"
                  "    return lock(*args, **kwargs)\n"
                  "room._rollover_lock = ready_lock\n"
                  "start = time.monotonic(); room.close(sys.argv[2], sys.argv[3], 'm1'); print(time.monotonic() - start)")
        def closing_carry(repo, state, target, bounded=True):
            if not racer:
                racer["process"] = subprocess.Popen(
                    [sys.executable, "-c", closer, str(Path(room.__file__).parent), str(where), closing, str(close_ready)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                deadline = time.monotonic() + 10
                while not close_ready.exists() and time.monotonic() < deadline:
                    assert racer["process"].poll() is None, "racing close exited before its lock"
                    time.sleep(.01)
                assert close_ready.exists(), "racing close did not reach its lock"
            return real_carry(repo, state, target, bounded)
        with bound(closing, "m1"), patch.object(room, "carry", side_effect=closing_carry):
            moved = room.rollover(where, closing, "m1")
            out, err = racer["process"].communicate(timeout=90)
        assert racer["process"].returncode == 0 and float(out.split()[-1]) < 20, (out, err)
        assert room.status(where, moved)["closed"] and room.selected(where, member(closing, "m1")["consumer"]) in (
            None, (None, None))
        # A finished call that answers a broadcast question between the first carry and the seal is carried with
        # its answer, as the sealed log's audience admits it; the question stays answered and the room writable.
        asked_all = fresh("m1", "m2", calls=[("call-f", "m1")])
        room.send(where, asked_all, "call-f", "m1", "Call exit=0; done", "handoff", message_id="result-call-f")
        with bound(asked_all, "m1"):
            room.acknowledge(where, asked_all, "m1", ["result-call-f"])
            question = room.send(where, asked_all, "m1", "all", "Anyone?", "question")["id"]
        answers = []
        def answering(repo, state, target, bounded=True):
            moved = real_carry(repo, state, target, bounded)
            if not answers:
                with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": asked_all,
                                             "OMS_ROOM_PARTICIPANT": "call-f", "OMS_ROOM_REPO": str(where)}):
                    answers.append(room.send(where, asked_all, "call-f", "m1", "Me", "answer", reply_to=question))
            return moved
        with bound(asked_all, "m1"), patch.object(room, "carry", side_effect=answering):
            moved = room.rollover(where, asked_all, "m1")
        assert room.status(where, moved)["carried"] and not room.status(where, moved)["open_questions"]
        # Its next rollover still carries that answer, though each carry narrowed the question's audience.
        with bound(asked_all, "m1"):
            room.send(where, asked_all, "m1", "m2", "Still writable", "note")
            chained = room.rollover(where, moved, "m1")
        assert room.status(where, chained)["carried"] and not room.status(where, chained)["open_questions"]
        assert answers[0]["id"] in {m["id"] for m in room.messages(where, chained)}
        # A carried broadcast reaches only its audience on a cursored read too: a main that rejoined after it reads
        # its own carried mail page by page and acknowledges all of it at once.
        paged = fresh("m1", "m2", "m3")
        notes = [room.send(where, paged, "m1", "m3", "Note %d" % n, "note")["id"] for n in range(3)]
        room.append(where, paged, {"kind": "leave", "participant": "m3"}, "Participant left")
        room.send(where, paged, "m1", "all", "Before m3 came back", "note")
        legacy_main_join(where, paged, "m3", "codex", "main")
        with bound(paged, "m1"):
            moved = room.rollover(where, paged, "m1")
        with bound(paged, "m3"):
            page = room.updates(where, moved, "m3", limit=2)
            rest = room.updates(where, moved, "m3", page["cursor"])
            read = [r["room_event"]["id"] for r in page["turns"] + rest["turns"]]
            assert page["has_more"] and read == notes, (page, rest)
            room.acknowledge(where, moved, "m3", read)
        # Nobody else takes the id of a rollover's note that unread mail stayed behind, so that note always lands.
        hidden = fresh("m1", "m2", calls=[("call-h", "m1")])
        note = "rollover-unread-" + hashlib.sha256(hidden.encode()).hexdigest()[:12]
        worker = patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": hidden,
                                         "OMS_ROOM_PARTICIPANT": "call-h", "OMS_ROOM_REPO": str(where)})
        for who, acting in (("call-h", worker), ("m2", bound(hidden, "m2"))):
            with acting:
                try:
                    room.send(where, hidden, who, "m1", "Harmless progress", "note", message_id=note)
                except ValueError as error:
                    assert "reserved for a rollover" in str(error), error
                else:
                    raise AssertionError("a participant took the id of a rollover's note")
        for n in range(3):
            room.send(where, hidden, "m1", "m2", "Unread %d" % n, "note")
        with bound(hidden, "m1"), patch.object(room, "MESSAGE_RESERVE", 1):
            moved = room.rollover(where, hidden, "m1")
        landed = next(m for m in room.messages(where, moved) if m["id"] == note)
        assert landed["sender"] == "m1" and "m2" in landed["targets"], landed
        # Native sessions handed on between the first carry and the seal are released before they are taken: a
        # main rebinds onto another main's old session, and a session dropped by a main that rejoined without
        # one goes to a new main.
        handed = fresh("m1", "m2", "x3")
        s1, s2, sx = (member(handed, who)["consumer"] for who in ("m1", "m2", "x3"))
        handoffs = []
        def handing(repo, state, target, bounded=True):
            moved = real_carry(repo, state, target, bounded)
            if not handoffs:
                handoffs.append(handed)
                with bound(handed, "m2"):
                    room.bind(where, handed, "m2", "sess-three", replaces=s2)
                with bound(handed, "m1"):
                    room.bind(where, handed, "m1", "sess-%s-m2" % handed, replaces=s1)
                with bound(handed, "x3"):
                    room.append(where, handed, {"kind": "leave", "participant": "x3"}, "Participant left")
                legacy_main_join(where, handed, "y4", "claude", "main", native_session="sess-%s-x3" % handed)
                legacy_main_join(where, handed, "x3", "codex", "main")
            return moved
        with bound(handed, "m1"), patch.object(room, "carry", side_effect=handing):
            moved = room.rollover(where, handed, "m1")
        people = {p["participant"]: p for p in room.status(where, moved)["participants"]}
        assert room.status(where, moved)["carried"] and people["m1"]["consumer"] == s2, people
        assert people["m2"]["consumer"] == hashlib.sha256(b"sess-three").hexdigest()[:32], people["m2"]
        assert people["y4"]["consumer"] == sx and not people["x3"].get("consumer"), people
        assert room.selected(where, sx) == (moved, "y4")
        for who in ("m1", "m2"):
            with bound(handed, who):
                room.send(where, handed, who, "x3", "Still writable", "note")
        # A worker's flood of open questions to its parent does not block the rollover.
        flooded = fresh("m1", calls=[("call-w", "m1")])
        worker_env = {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": flooded, "OMS_ROOM_PARTICIPANT": "call-w",
                      "OMS_ROOM_REPO": str(where)}
        with patch.dict(os.environ, worker_env):
            for n in range(3):
                room.send(where, flooded, "call-w", "m1", "Question %d" % n, "question")
            try:
                room.rollover(where, flooded)
            except ValueError as error:
                assert "worker cannot roll" in str(error), error
        with bound(flooded, "m1"), patch.object(room, "CARRY_MESSAGES", 1):
            moved = room.rollover(where, flooded, "m1")
        assert len(room.status(where, moved)["open_questions"]) == 3
        # A worker cannot take its room's derivable successor id ahead of the rollover, nor write plain rows
        # (a raw close among them) into a room log; oms room close refuses it the same way.
        squatted = fresh("m1", calls=[("call-s", "m1")])
        squatter = dict(os.environ, OMS_HARNESS_CHILD="1", OMS_ROOM_ID=squatted, OMS_ROOM_PARTICIPANT="call-s",
                        OMS_ROOM_REPO=str(where))
        for verb in (["new", "--id", room.successor_id(squatted), "--topic", "x"],
                     ["new", "--id", room.successor_id(squatted), "--live"],
                     ["new", "--id", room.successor_id(squatted).upper(), "--live"],
                     ["append", "--id", squatted, "--role", "note", "--text", "plain"],
                     ["close", "--id", squatted]):
            refused = subprocess.run(["bash", str(panel.ENTRY), "thread", "--repo", str(where)] + verb, env=squatter,
                                     capture_output=True, text=True, timeout=60)
            assert refused.returncode and ("worker cannot create a room" in refused.stderr
                                           or "admitted room participant" in refused.stderr), (verb, refused)
        assert not (where / ".oms/threads" / (room.successor_id(squatted) + ".jsonl")).exists()
        assert not room.status(where, squatted)["closed"]
        # Nor a plain row while the rollover creates the successor: neither in its empty file nor after its topic.
        successor = where / ".oms/threads" / (room.successor_id(squatted) + ".jsonl")
        successor.write_bytes(b"")
        bare = {k: v for k, v in squatter.items() if not k.startswith("OMS_ROOM_")}
        squat = ["bash", str(panel.ENTRY), "thread", "--repo", str(where), "append", "--id",
                 room.successor_id(squatted), "--role", "note", "--text", "squat"]
        squats, real_command = [subprocess.run(squat, env=bare, capture_output=True, text=True, timeout=60)], room.command
        def squatting(repo, *args, env=None):
            out = real_command(repo, *args, env=env)
            if args[:3] == ("new", "--id", room.successor_id(squatted)):
                squats.append(subprocess.run(squat, env=squatter, capture_output=True, text=True, timeout=60))
            return out
        with bound(squatted, "m1"), patch.object(room, "command", side_effect=squatting):
            moved = room.rollover(where, squatted, "m1")
        assert len(squats) == 2 and all(s.returncode and "admitted room participant" in s.stderr for s in squats), squats
        # A worker's message through another spelling of its room id, one log on a case-insensitive filesystem, is
        # refused, so every row keeps the log's own id and the room stays readable.
        real_path = room.thread_live.thread_path
        with patch.object(room.thread_live, "thread_path", side_effect=lambda repo, ident: real_path(repo, ident.lower())), \
                patch.dict(os.environ, dict(squatter, OMS_TH_FILE=str(where / ".oms/threads" / (moved + ".jsonl")),
                                            OMS_TH_ID=moved.upper(), OMS_TH_ROLE="note", OMS_TH_ROOM_EVENT=json.dumps({
                                                "kind": "message", "id": "cv1", "sender": "call-s", "recipient": "m1",
                                                "message_kind": "note"}))):
            try:
                room.thread_live.append_from_env()
            except ValueError as error:
                assert "does not match its log" in str(error), error
            else:
                raise AssertionError("a row was written under another spelling of its room id")
        assert "cv1" not in {m["id"] for m in room.messages(where, moved)}
        refused = subprocess.run(["bash", str(panel.ENTRY), "thread", "--repo", str(where), "close", "--id", moved],
                                 env=squatter, capture_output=True, text=True, timeout=60)
        assert refused.returncode and not room.status(where, moved)["closed"], refused
        # The successor keeps the sealed room's creator: the main that rolled it over gains no close right, and
        # a rebound main keeps the session it first joined with, so resume ownership still knows that session.
        created = room.create(where, title="Creator room", creator="c0")
        for who in ("m1", "m2", "m3"):
            legacy_main_join(where, created, who, "codex", "main", native_session="sess-%s-%s" % (created, who))
        first = member(created, "m1")["consumer"]
        # A main that rejoined without a native session stays unbound; its released session is not revived.
        released = member(created, "m3")["consumer"]
        room.append(where, created, {"kind": "leave", "participant": "m3"}, "Participant left")
        legacy_main_join(where, created, "m3", "codex", "main")
        legacy_main_join(where, created, "pi9", "pi", "main", native_session="pi-a")
        room.append(where, created, {"kind": "leave", "participant": "pi9"}, "Participant left")
        legacy_main_join(where, created, "pi9", "pi", "main", native_session="pi-b")
        with bound(created, "m1"):
            room.bind(where, created, "m1", "sess-rebound", replaces=first)
        with bound(created, "m2"):
            moved = room.rollover(where, created, "m2")
            assert room.status(where, moved)["creator"] == "c0"
            room.append(where, created, {"kind": "leave", "participant": "m2"}, "Participant left")
            try:
                room.close(where, created, "m2")
            except ValueError as error:
                assert "has not joined" in str(error), error
            else:
                raise AssertionError("the main that rolled a room over kept a creator's close right")
        rebound = member(moved, "m1")
        assert rebound["initial_consumer"] == first and rebound["consumer"] != first, rebound
        assert member(moved, "pi9")["consumer"] == hashlib.sha256(b"pi-b").hexdigest()[:32]
        assert not member(moved, "m3").get("consumer") and room.selected(where, released) in (None, (None, None))
        assert room.selected(where, first) == (None, None) and any(
            p.get("initial_consumer") == first for s in room.discover(where)[0] if s["id"] == moved
            for p in s["participants"])
        room.close(where, created, "c0")
        assert room.status(where, moved)["closed"]
        # Attention keeps what still waits on the person one and two rollovers on: finished calls whose results the
        # main read (so not carried) with a patch to admit or a review to decide, a task its ended calls still
        # hold and the sealed room's failed wake-ups. Attempts launched in the first room keep their metadata on
        # the newest board, a carried call's among them, from the dashboard rows and from the full ledger alike.
        # A patch already admitted and a review already accepted are only past; a call carried on rejoins with a
        # new seq yet the later launch still supersedes it.
        owing = fresh("m1", calls=[("call-adm", "m1"), ("call-rev", "m1"), ("call-held", "m1"), ("call-run", "m1"),
                                   ("call-x", "m1"), ("call-y", "m1"), ("call-landed", "m1"), ("call-decided", "m1")])
        for who, code in (("call-adm", 0), ("call-rev", 0), ("call-held", 1), ("call-x", 1), ("call-y", 1),
                          ("call-landed", 0), ("call-decided", 0)):
            room.send(where, owing, who, "m1", "Call exit=%d; done" % code, "handoff", message_id="result-" + who)
        claude_app_notify = __import__("claude_app_notify")
        with bound(owing, "m1"):
            room.acknowledge(where, owing, "m1", ["result-" + who for who in (
                "call-adm", "call-rev", "call-held", "call-y", "call-landed", "call-decided")])
            with patch.object(codex_app_notify, "deliver", return_value={"status": "failed"}), \
                    patch.object(claude_app_notify, "deliver", return_value={"status": "failed"}):
                room.publish(where, owing)
                room.publish(where, owing, app="claude", recipient="m1", allow_wakeup=True)
        launched = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        shown_rows = [{"attempt_id": "att-" + who, "state": "review", "task_id": "task-" + who, "updated_at": launched,
                       "panel": {"room_id": owing, "room_participant": who, "role": "worker", "label": "Launched " + who,
                                 "location": "worktree:" + who}} for who in ("call-rev", "call-run")]
        shown_rows.append({"attempt_id": "att-council", "tool": "panel-council", "task_id": "task-council",
                           "state": "done", "updated_at": launched,
                           "panel": {"room_id": owing, "room_participant": "m1", "role": "advisor"}})
        ledger = [{"attempt_id": "att-" + who, "state": state, "task_id": "task-" + who, "updated_at": launched,
                   "refs": dict({"panel_room_id": owing, "panel_room_participant": who, "panel_role": "worker"}, **refs)}
                  for who, state, refs in (("call-adm", "done", {"panel_access": "write"}), ("call-held", "failed", {}),
                                           ("call-landed", "done", {"panel_access": "write"}), ("call-decided", "review", {}))]
        ledger += [{"attempt_id": "att-" + who, "state": "failed", "task_id": "task-pair", "updated_at": launched,
                    "refs": {"panel_room_id": owing, "panel_room_participant": who, "panel_role": "worker"}}
                   for who in ("call-x", "call-y")]
        tasks = [{"id": "task-call-held", "state": "running", "claimed_by": "m1"},
                 {"id": "task-pair", "state": "running", "claimed_by": "m1"}]
        dashboard_state = {"repo": {"name": "x"}, "attempts": {"recent": shown_rows},
                           "plan": {"present": True, "goal": "Owed work", "tasks": tasks}}

        def evidence(command, **kwargs):
            found = {"tasks": tasks} if "agent-plan" in command else ledger if "agent-events" in command else []
            return subprocess.CompletedProcess(command, 0, json.dumps(found), "")

        def collect(target):
            with patch.object(panel, "dashboard", side_effect=lambda *a, **k: (deepcopy(dashboard_state), 0)), \
                    patch.object(panel, "managed_session", return_value=False), \
                    patch.object(panel, "outcomes", side_effect=lambda repo, ids, **k: {
                        task: "accepted" for task in ids if task == "task-call-decided"}), \
                    patch.object(panel_results, "pending_admissions",
                                 side_effect=lambda repo, ids, _tick=None: {"task-call-adm"} & set(ids)), \
                    patch.object(panel.subprocess, "run", side_effect=evidence):
                return panel.snapshot(where, target)[0]
        for hops in (1, 2):
            with bound(owing, "m1"):
                current = room.rollover(where, room.resolve(where, owing, fresh=True), "m1")
            joined = {p["participant"] for p in room.status(where, current)["participants"]}
            assert "call-run" in joined and ("call-x" in joined) == (hops == 1), joined
            assert not {"call-adm", "call-rev", "call-held", "call-y", "call-landed", "call-decided"} & joined, joined
            board = collect(current)
            assert board["room"]["id"] == current and len(board["room"]["lineage"]) == hops, board["room"].get("lineage")
            assert board["awaiting_admission"] == ["call-adm"], board.get("awaiting_admission")
            queue = __import__("panel_view").attention_items(board)
            assert sorted((item["cls"], item["what"]) for item in queue) == [
                ("decide", "awaiting decision: Launched call-rev"), ("decide", "awaiting decision: Launched call-run"),
                ("decide", "patch awaits admission: call-adm"), ("info", ""),
                ("stale", "workers ended, still held"), ("stale", "workers ended, still held")], (hops, queue)
            assert next(item["who"] for item in queue if item["cls"] == "info") == "Main wake-up delivery failed"
            drawn = {m["participant"]: m for m in room_view.nodes(board)}
            assert [drawn[who]["location"] for who in ("call-rev", "call-run")] == ["worktree:call-rev", "worktree:call-run"]
            assert drawn["call-adm"]["access"] == "write" and drawn["call-held"]["state"] == "failed", drawn
            assert drawn["call-x"].get("superseded") is True and not drawn["call-y"].get("superseded"), drawn
            assert not {"call-landed", "call-decided"} & set(drawn), drawn
            assert room_view.held_split(board) == (set(), {"task-call-held", "task-pair"})
            assert [d["task"] for d in panel_debate.debates(board)] == ["task-council"]
            # The Past tab lists what the board no longer draws, once; a call still owed stays off it.
            past_navigation = {"tab": "past"}
            room_view.tab_box(board, 100, 12, past_navigation)
            past = past_navigation["past_ids"]
            assert len(past) == len(set(past)) and {("result", "call-landed"), ("result", "call-decided")} <= set(past)
            assert not {("result", "call-adm"), ("result", "call-rev"), ("result", "call-run")} & set(past), past
            if hops == 1:
                # The newest room's own receipt replaces the sealed one for its target; one room on, the nearer
                # room's receipt still wins, while the first room's other failure stays two rooms on.
                assert set(board["room"]["predecessor_publications"]) == {"codex", "claude.m1"}
                with bound(current, "m1"), patch.object(codex_app_notify, "deliver", return_value={"status": "persisted"}):
                    room.publish(where, current)
                    room.acknowledge(where, current, "m1", ["result-call-x"])
                assert set(collect(current)["room"]["predecessor_publications"]) == {"claude.m1"}
            else:
                assert {target: receipt["status"] for target, receipt in board["room"]["predecessor_publications"].items()} == {
                    "codex": "persisted", "claude.m1": "failed"}
                # A call read only in the nearer room is read; the chain stops at a room that does not continue in
                # the later one, and after MAX_HOPS rooms.
                assert "result-call-x" in room_view.handled_results(board["room"])
                assert room.lineage(where, {"id": "room-elsewhere", "predecessor": board["room"]["lineage"][0]}) == []
                with patch.object(room, "MAX_HOPS", 1):
                    assert [old for old, *_ in room.lineage(where, board["room"])] == board["room"]["lineage"][:1]
        # A later round in the newest room supersedes the held call, though seq numbers restart in every room: its
        # own failure, not the stale hold, is what the person sees.
        room.join(where, current, "call-again", "codex", "worker", parent="m1")
        ledger.append({"attempt_id": "att-call-again", "state": "failed", "task_id": "task-call-held", "updated_at": launched,
                       "refs": {"panel_room_id": current, "panel_room_participant": "call-again", "panel_role": "worker"}})
        board = collect(current)
        drawn = {m["participant"]: m for m in room_view.nodes(board)}
        assert drawn["call-held"].get("superseded") is True and not drawn["call-again"].get("superseded"), drawn
        queue = __import__("panel_view").attention_items(board)
        assert ("failed", "failed: call-again") in {(item["cls"], item["what"]) for item in queue}, queue
        assert [item["row_id"] for item in queue if item["cls"] == "stale"] == [("task", "task-pair")], queue
    for mode, panel_env in (("stripped", False), ("panel", True)):
        rollover_regressions(temporary / ("rollover regressions " + mode), panel_env)
# A selected room that rolled over names its successor: the new main and the session record get the successor.
with patch.dict(os.environ, {"OMS_ROOM_ID": old, "OMS_PANEL_SESSION": "roll-session"}), \
        patch.object(panel, "managed_session", return_value=True), \
        patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as recorded_room:
    assert panel.panel_room(roll) == third
    assert recorded_room.call_args.args[0][-3:] == ["=roll-session", "OMS_ROOM_ID", third], recorded_room.call_args
# The running panel's tmux bindings follow the successor; a window naming another room is left alone.
tmux_calls = []
def tmux_fake(command, **kwargs):
    tmux_calls.append(command)
    return subprocess.CompletedProcess(command, 0, "@1\t%s\n@2\troom-elsewhere\n" % old if "list-windows" in command else "", "")
with patch.object(panel, "recorded_room", return_value=old), patch.object(panel.shutil, "which", return_value="tmux"), \
        patch.object(panel.subprocess, "run", side_effect=tmux_fake):
    panel._adopt_successor(roll, "roll-session")
assert [c[-3:] for c in tmux_calls if "list-windows" not in c] == [
    ["=roll-session", "OMS_ROOM_ID", third], ["@1", "@oms_panel_room", third]], tmux_calls
# Publication receipts outlive a rollover: an uncertain send needs --retry from the successor too, and a
# receipt landing after the seal still matches the intent the sealed room holds.
import codex_app_notify
pub = temporary / "publication rollover repo"
pub.mkdir()
subprocess.run(["git", "init", "-q", str(pub)], check=True)
with patch.dict(os.environ, {}):
    for name in [n for n in os.environ if n.startswith(("OMS_ROOM_", "OMS_PANEL_")) or n in (
            "OMS_HARNESS_CHILD", "OMS_HARNESS_DELEGATE_DEPTH", "TMUX", "TMUX_PANE")]:
        del os.environ[name]
    first = room.create(pub, title="Publication room")
    room.join(pub, first, "pub-main", "codex", "main")
    sent = []
    def unknown(repo, text):
        sent.append(text)
        return {"status": "submitted", "delivery_unknown": True}
    with patch.object(codex_app_notify, "deliver", unknown):
        assert room.publish(pub, first)["deduplicated"] is False
        room.rollover(pub, first)
        assert room.publish(pub, first)["deduplicated"] is True and len(sent) == 1, sent
        assert room.publish(pub, first, retry=True)["deduplicated"] is False and len(sent) == 2, sent
    second = room.create(pub, title="Late receipt room")
    room.join(pub, second, "pub-main", "codex", "main")
    def seals_first(repo, text):
        room.rollover(repo, second)
        return {"status": "persisted", "delivery_unknown": False}
    with patch.object(codex_app_notify, "deliver", seals_first):
        room.publish(pub, second)
    assert room.status(pub, room.successor_id(second))["publications"]["codex"]["status"] == "persisted"
