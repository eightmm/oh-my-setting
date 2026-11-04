execution_repo = temporary / "verification-execution-root"
execution_repo.mkdir()
import panel_dispatch as dispatch
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import Mock, patch
import io
from concurrent.futures import ThreadPoolExecutor

# BEGIN continuation round delta
delta_task = "delta-task"
delta_attempts = [dict(task_id=delta_task, attempt_id="delta-%s" % n, state="done",
                       refs={"panel_access": "write", "panel_role": "worker"}) for n in (1, 2)]
delta_patches = [
    b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+first\n",
    b"diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+second\n"
    b"diff --git a/b.txt b/b.txt\n--- /dev/null\n+++ b/b.txt\n@@ -0,0 +1 @@\n+new\n",
]
delta_rows = []
for index, data in enumerate(delta_patches):
    path = project / (".oms/artifacts/delta-%s.patch" % index)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    delta_rows.append(dict(task_id=delta_task, kind="delegate", attempt_id="delta-%s" % (index + 1),
                           patch=path.relative_to(project).as_posix(), patch_sha256=hashlib.sha256(data).hexdigest()))
with patch.object(saved, "_records", return_value=(delta_attempts, delta_rows)), \
        patch.object(saved, "_worker_owns", return_value=[]):
    delta = saved.results(project, delta_task)["rows"][0]["round_delta"]
    assert delta == {"since_round": 1, "count": 2, "files": [
        {"path": "a.txt", "added": 1, "removed": 1},
        {"path": "b.txt", "status": "new in this round"}]}, delta
    import room_view
    rendered = room_view.changed_lines({"round_delta": delta})
    assert rendered == ["Since round 1: 2 files changed", "  a.txt +1 -1", "  b.txt new in this round"], rendered
    assert room_view.changed_lines({"round_delta": {"issue": "skipped (too large)"}}) == ["round delta: skipped (too large)"]
    assert room_view.changed_lines({}) == []
with patch.object(saved, "_records", return_value=(delta_attempts[:1], delta_rows[:1])):
    assert "round_delta" not in saved.results(project, delta_task)["rows"][0]
with patch.object(saved, "_records", return_value=(delta_attempts, [delta_rows[0], dict(delta_rows[1], patch_sha256="bad")])):
    assert saved.results(project, delta_task)["rows"][0]["round_delta"] == {"issue": "unavailable"}
with patch.object(saved, "_records", return_value=(delta_attempts, [delta_rows[0], dict(delta_rows[1], patch=".oms/artifacts/missing.patch")])):
    assert saved.results(project, delta_task)["rows"][0]["round_delta"] == {"issue": "unavailable"}
with patch.object(saved, "verified_patch", side_effect=[delta_patches[1], delta_patches[0]]):
    delta = saved._round_delta(project, delta_attempts, delta_rows)
    assert delta["files"][1] == {"path": "b.txt", "status": "dropped this round"}, delta
with patch.object(saved, "verified_patch", side_effect=[b"", b""]):
    assert saved._round_delta(project, delta_attempts, delta_rows) == {"since_round": 1, "count": 0, "files": []}
for size, issue in ((saved.ROUND_DELTA_MAX, None), (saved.ROUND_DELTA_MAX + 1, "skipped (too large)")):
    body = b"diff --git a/a b/a\n--- /dev/null\n+++ b/a\n@@ -0,0 +1 @@\n+"
    body += b"x" * (size - len(body) - 1) + b"\n"
    with patch.object(saved, "verified_patch", side_effect=[b"", body]):
        result = saved._round_delta(project, delta_attempts, delta_rows)
        assert result.get("issue") == issue, result
for bad in (b"nonsense\n", b"diff --git a/a b/a\n@@ -1 +1 @@\n+truncated\n",
            b"diff --git a/a b/a\nGIT binary patch\n"):
    with patch.object(saved, "verified_patch", side_effect=[b"", bad]):
        assert saved._round_delta(project, delta_attempts, delta_rows) == {"issue": "unavailable"}
no_newline = (b"diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n"
              b"\\ No newline at end of file\n+new\n\\ No newline at end of file\n")
assert {line: n for line, n in saved._patch_line_balance(no_newline)["a"].items() if n} == {b"old": -1, b"new": 1}
context_marker = b"diff --git a/a b/a\n@@ -2 +2 @@\n same\n\\ No newline at end of file\n"
assert not saved._patch_line_balance(context_marker)["a"]
carriage = b"diff --git a/a b/a\n@@ -1 +1 @@\n-before\rpart\n+after\rpart\n"
assert saved._patch_line_balance(carriage)["a"][b"after\rpart\n"] == 1
middle = dict(delta_attempts[0], attempt_id="delta-missing")
with patch.object(saved, "verified_patch", side_effect=delta_patches):
    assert saved._round_delta(project, [delta_attempts[0], middle, delta_attempts[1]], delta_rows)["since_round"] == 1
# END continuation round delta

# BEGIN continued acceptance regression
with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": "main"}):
    task = {"state": "running", "verify": "true", "lease_id": "lease", "plan_id": "plan"}
    main = {"attempt_id": "main", "tool": "panel-main", "provider": "codex"}
    first = {"task_id": "chain", "attempt_id": "first", "parent_attempt_id": "main",
             "created_at": "1", "refs": {"panel_role": "worker", "panel_access": "write",
                                           "panel_lease_id": "lease"}}
    final = dict(first, attempt_id="final", created_at="2",
                 refs=dict(first["refs"], panel_continues="first"))
    row = {"kind": "delegate", "task_id": "chain", "attempt_id": "final", "exit": 0,
           "plan_id": "plan", "patch_sha256": "final-digest"}
    review = {"kind": "review-outcome", "task_id": "chain", "exit": 0,
              "review": {"candidate": {"patch_sha256": "final-digest"}, "gate_verify_exit": 0,
                         "seats": [{"verdict": "pass"}]}}
    with patch.object(saved, "_owned_task", return_value=task), \
            patch.object(saved, "_plan_bindings", return_value={}), \
            patch.object(saved, "_records", return_value=([main, first, final], [row, review])), \
            patch.object(saved, "_admission", return_value=None), \
            patch.object(saved, "_empty_salvaged_attempt", return_value=False), \
            patch.object(saved, "_worker_owns", return_value=["value.txt"]), \
            patch.object(saved, "verified_patch", return_value=b""), \
            patch.object(saved, "_indexed"):
        assert saved._accept_preflight(project, project, "codex", "chain", "true")["worker"] == final
        for ref in (None, "", "0", "malformed"):
            unrelated = dict(final, refs=dict(final["refs"], panel_continues=ref))
            with patch.object(saved, "_records", return_value=([main, first, unrelated], [row, review])):
                try:
                    saved._accept_preflight(project, project, "codex", "chain", "true")
                    raise AssertionError("unrelated worker accepted")
                except ValueError as error:
                    assert "unique worker candidate" in str(error), error
        older_review = dict(review, review=dict(review["review"], candidate={"patch_sha256": "old-digest"}))
        with patch.object(saved, "_records", return_value=([main, first, final], [row, older_review])):
            try:
                saved._accept_preflight(project, project, "codex", "chain", "true")
                raise AssertionError("unreviewed final worker accepted")
            except ValueError as error:
                assert "passing review gate" in str(error), error
    selected, admissions, _ = saved._write_candidates(project, [first, final], [{"event_id": "old"}, None], [], "codex")
    assert selected == [first, final] and admissions == [{"event_id": "old"}, None]
# END continued acceptance regression


def exercise_continued_stage_rollup(repo):
    task = "continued-stage"
    attempts = [dict(task_id=task, attempt_id="round-%s" % n, state="done",
                     refs=({"panel_continues": "round-%s" % (n - 1)} if n > 1 else {}))
                for n in range(1, 4)]
    calls = [dict(task_id=task, kind="delegate", attempt_id="round-%s" % n,
                  exit=1 if n == 1 else 0, verify_exit=1 if n == 1 else 0)
             for n in range(1, 4)]
    with patch.object(saved, "_records", return_value=(attempts, calls)):
        row = saved.results(repo, task_id=task)["rows"][0]
    assert row["stages"]["implement"]["state"] == "passed", row
    assert row["stages"]["verify"]["state"] == "passed", row
    assert row["next_action"] == "record main decision", row
    assert len(row["calls"]) == 3 and not any(c.get("superseded") for c in row["calls"]), row
    for ref in (None, "", "0", "malformed"):
        independent = [attempts[0], dict(attempts[1], refs={"panel_continues": ref})]
        with patch.object(saved, "_records", return_value=(independent, calls[:2])):
            row = saved.results(repo, task_id=task)["rows"][0]
        assert row["stages"]["implement"]["state"] == "failed", row
        assert row["stages"]["verify"]["state"] == "failed", row
        assert row["next_action"] == "retry implement", row
    verify_calls = [dict(calls[0], exit=0), calls[1]]
    with patch.object(saved, "_records", return_value=(attempts[:2], verify_calls)):
        row = saved.results(repo, task_id=task)["rows"][0]
    assert row["stages"]["verify"]["state"] == "passed", row
    # A fresh write re-dispatch (no panel_continues) that succeeds retires the earlier failure; a later failure does not.
    write = {"panel_access": "write", "panel_role": "worker", "panel_purpose": "implement"}
    fresh = [dict(attempts[0], refs=dict(write)), dict(attempts[1], refs=dict(write)),
             dict(attempts[2], refs=dict(write, panel_continues="round-2"))]

    def rollup(attempt_list, call_list, scopes=None):
        scope = lambda _repo, worker, **kwargs: (scopes or {}).get(worker["attempt_id"], ["value.txt"])
        with patch.object(saved, "_records", return_value=(attempt_list, call_list)), \
                patch.object(saved, "_worker_owns", side_effect=scope):
            return saved.results(repo, task_id=task)["rows"][0]

    for exits, state in (((1, 0, 0), "passed"), ((1, 1, 0), "passed"), ((0, 0, 1), "failed"),
                         ((0, 1, 1), "failed"), ((1, 1, 1), "failed"), ((0, 1, 0), "passed")):
        rounds = [dict(task_id=task, kind="delegate", attempt_id="round-%s" % (n + 1), exit=e, verify_exit=e)
                  for n, e in enumerate(exits)]
        row = rollup(fresh, rounds)
        assert row["stages"]["implement"]["state"] == state, (exits, row)
        assert row["stages"]["verify"]["state"] == state, (exits, row)
        assert (row["next_action"] == "retry implement") == (state == "failed"), (exits, row)
    # Only a later write round with the same purpose and a covering scope retires a failure.
    failed = dict(task_id=task, kind="delegate", attempt_id="round-1", exit=1, verify_exit=1)
    ok = lambda ident: dict(failed, attempt_id=ident, exit=0, verify_exit=0)
    advisor = dict(attempts[1], refs={"panel_access": "read", "panel_role": "advisor", "panel_purpose": "advise"})
    sibling = dict(attempts[1], refs=dict(write))
    for later, later_call, scopes in (
            (advisor, dict(ok("round-2"), kind="call"), None),
            (advisor, ok("round-2"), None),
            (dict(advisor, refs=dict(advisor["refs"], panel_purpose="implement")), dict(ok("round-2"), kind="call"), None),
            (sibling, ok("round-2"), {"round-1": ["a.txt"], "round-2": ["b.txt"]}),
            (sibling, ok("round-2"), {"round-1": []}),
            (dict(sibling, refs=dict(write, panel_purpose="other")), ok("round-2"), None)):
        row = rollup([dict(attempts[0], refs=dict(write)), later], [failed, later_call], scopes)
        assert row["stages"]["implement"]["state"] == "failed", (later, row)
        assert row["next_action"] == "retry implement", (later, row)
    row = rollup([dict(attempts[0], refs=dict(write)), sibling], [failed, ok("round-2")],
                 {"round-1": ["a.txt"], "round-2": ["a.txt", "b.txt"]})
    assert row["stages"]["implement"]["state"] == "passed", row
    # Round 1 failed and salvaged, round 2 fresh, round 3 continues round 2: accept sees round 3 only.
    def worker(n, state="done", refs=None):
        return dict(task_id=task, attempt_id="round-%s" % n, parent_attempt_id="main", state=state,
                    created_at="2026-11-02T00:00:0%sZ" % n, refs=refs or {"panel_purpose": "implement"})
    def delegate(n, code):
        return dict(kind="delegate", task_id=task, attempt_id="round-%s" % n, exit=code,
                    patch="p%s.patch" % n, patch_sha256="d" * 63 + str(n))
    first, second = worker(1, state="failed"), worker(2)
    third = worker(3, refs={"panel_purpose": "implement", "panel_continues": "round-2"})
    with patch.object(saved, "_worker_owns", return_value=["value.txt"]):
        chosen, _, dropped = saved._write_candidates(repo, [first, second, third], [None] * 3, [], "codex",
                                                    rows=[delegate(1, 1), delegate(2, 0), delegate(3, 0)])
        assert chosen == [third] and len(dropped) == 2, (chosen, dropped)
        for rows in ([delegate(1, 1), delegate(2, 1), delegate(3, 1)], [delegate(1, 1)],
                     [dict(delegate(2, 0), patch=None)]):
            chosen, _, dropped = saved._write_candidates(repo, [first, second], [None] * 2, [], "codex", rows=rows)
            assert chosen == [first, second] and not dropped, (rows, chosen)


# BEGIN results task index
# Frozen output covers interleaved task/attempt rows, duplicate rows, whole-main
# children, participant scope and both pagination windows without timing assertions.
index_attempts, index_rows, index_results = [], [], {}
index_members = []
for n in range(24):
    ident, attempt = "indexed-%02d" % n, "worker-%02d" % n
    index_attempts.append(dict(task_id=ident, attempt_id=attempt, state="done", updated_at=str(n),
        refs=dict(panel_access="write", panel_role="worker", panel_room_id="index-room",
                  panel_room_participant=attempt, panel_alias="W%s" % n)))
    index_members.append(dict(participant=attempt, owns=["value.txt"]))
    index_results[ident] = dict(task_id=ident, revision="a" * 24, outcome="completed",
                              main_attempt_id="index-main", summary=ident)
    index_rows.append(dict(task_id=ident, attempt_id=attempt, kind="delegate", exit=0,
                           patch="fixture.patch", patch_sha256="b" * 64))
index_attempts.append(dict(attempt_id="unassigned", parent_attempt_id="index-main", refs={}))
index_results["whole-index"] = dict(task_id="whole-index", revision="a" * 24,
    outcome="completed", main_attempt_id="index-main", summary="Whole main")
index_results["preflight-index"] = dict(task_id="preflight-index", revision="a" * 24,
    outcome="completed", summary="Preflight")
index_rows += [dict(task_id=ident, kind="panel-result", ts="z" + ident)
               for ident in reversed(index_results)]
index_rows += [dict(kind="call", attempt_id="unassigned", exit=0),
               dict(task_id="whole-index", kind="call", attempt_id="unassigned", exit=0),
               dict(task_id="indexed-00", kind="call", attempt_id="unassigned", exit=0)]
index_rows.append(index_rows[-2])
# First matching attempt supplies aliases even if an ID is repeated later.
index_attempts.append(dict(index_attempts[0], refs=dict(index_attempts[0]["refs"], panel_alias="later")))
index_pages = [{}, dict(limit=25), dict(limit=1), dict(offset=24, limit=1), dict(offset=25),
               dict(task_id="indexed-00"), dict(task_id="whole-index"), dict(task_id="missing"),
               dict(room_participant="worker-00"), dict(room_participant="missing"),
               dict(include_preflight=True)]
index_reads = []
class IndexRow(dict):
    def get(self, key, default=None):
        if key == "task_id":
            index_reads.append(key)
        return super().get(key, default)

with patch.object(saved, "_records", return_value=(index_attempts, index_rows)), \
        patch.object(saved, "_load_result", side_effect=lambda repo, row: index_results[row["task_id"]]), \
        patch.object(saved, "_last_delivery", return_value=None), \
        patch.object(saved, "_diffstat", return_value=dict(files=[], count=0, added=0, removed=0)), \
        patch.object(saved, "verified_patch", return_value=b""), \
        patch.object(saved, "_round_delta", return_value=None), \
        patch.object(saved.room, "status", return_value=dict(participants=index_members)) as index_room:
    index_reports = [saved.results(project, **options) for options in index_pages]
    index_encoded = json.dumps(index_reports, ensure_ascii=False, separators=(",", ":")).encode()
    # Digest of the original reader's complete serialized reports for these selections.
    assert hashlib.sha256(index_encoded).hexdigest() == "1acecc5d82e9a9affc9ecc67ba158b4f329b1388e05172ba416a0b699bffd850"
    assert [row["task_id"] for row in index_reports[0]["rows"]][:1] == ["whole-index"]
    assert len(index_reports[0]["rows"]) == 20 and index_reports[0]["truncated"]
    assert len(index_reports[1]["rows"]) == 25 and not index_reports[1]["truncated"]
    assert len(index_reports[6]["rows"][0]["calls"]) == 4
    index_room.reset_mock()
    saved.results(project)
    assert index_room.call_count == 1, index_room.call_count
    # A second call reads current room evidence; no process-wide cache survives.
    saved.results(project)
    assert index_room.call_count == 2, index_room.call_count
    with ThreadPoolExecutor(max_workers=2) as index_pool:
        index_parallel = list(index_pool.map(lambda _: saved.results(project), range(2)))
    assert index_parallel == [index_reports[0], index_reports[0]]
    assert index_room.call_count == 4, index_room.call_count
    with patch.object(saved, "_records", return_value=(index_attempts, [IndexRow(row) for row in index_rows])):
        saved.results(project)
    assert len(index_reads) <= 4 * len(index_rows), len(index_reads)
    with patch.object(saved, "_records", return_value=([], [])):
        assert saved.results(project)["rows"] == []
    with patch.object(saved, "_records", return_value=([], [dict(task_id=[], attempt_id=[])])):
        assert saved.results(project)["rows"] == []
    with patch.object(saved, "_records", return_value=(index_attempts, index_rows + [
            dict(task_id="whole-index", attempt_id=[])])):
        assert saved.results(project, task_id="whole-index") == index_reports[6]
        try:
            saved.results(project, task_id="indexed-00")
            raise AssertionError("malformed linked ID accepted")
        except TypeError:
            pass
    for options in (dict(limit=0), dict(offset=-1), dict(limit="1"), dict(offset=None), dict(task_id="bad/id")):
        try:
            saved.results(project, **options)
            raise AssertionError("invalid results selection accepted")
        except ValueError:
            pass
    assert saved._worker_owns(project, {}, _rooms={}) == []
    assert saved._worker_owns(project, index_attempts[0]) == ["value.txt"]
    index_room.assert_called_with(project, "index-room", exact=True)
    with patch.object(saved.room, "status", side_effect=[ValueError("unavailable"), dict(participants=index_members)]):
        index_cache = {}
        try:
            saved._worker_owns(project, index_attempts[0], _rooms=index_cache)
            raise AssertionError("unavailable scope accepted")
        except ValueError:
            pass
        assert not index_cache
        assert saved._worker_owns(project, index_attempts[0], _rooms=index_cache) == ["value.txt"]
        try:
            saved._worker_owns(project, dict(refs=dict(panel_room_id="index-room", panel_room_participant="missing")),
                               _rooms=index_cache)
            raise AssertionError("missing participant accepted")
        except ValueError:
            pass
    exercise_continued_stage_rollup(project)
# END results task index


def exercise_pi_cooldown_routing():
    import panel_routing
    from datetime import datetime, timezone, timedelta

    now = datetime.now(timezone.utc).replace(microsecond=0)
    model = "openrouter/thinkingmachines/inkling:free"
    five_min_ago = (now - timedelta(seconds=300)).strftime("%Y-%m-%dT%H:%M:%SZ")
    thirty_min_ago = (now - timedelta(seconds=1800)).strftime("%Y-%m-%dT%H:%M:%SZ")
    cooling = {"pi": {"cooldown": {model: five_min_ago}}}
    assert allocate("codex", installed={"pi", "claude"}, provider_status=cooling)["provider"] == "claude"
    assert allocate("codex", installed={"pi"}, provider_status=cooling)["provider"] == "pi"

    def reading(entries):
        ledger = [{"refs": dict(provider_error_provider="pi", provider_error="capacity",
                                 provider_capacity_model=name, provider_capacity_at=at)}
                  for name, at in entries]
        with patch("terminal_panel.events", return_value=json.dumps(ledger)), \
                patch.dict(os.environ, {"OMS_ROOM_ID": ""}), \
                patch("datetime.datetime", wraps=datetime) as clock:
            clock.now.return_value = now
            return dispatch.routing_status(project)

    expired = reading([(model, thirty_min_ago)])
    assert allocate("codex", installed={"pi", "claude"}, provider_status=expired)["provider"] == "pi"
    routes = deepcopy(panel_routing.policy())
    other = "openrouter/other:free"
    routes["tiers"]["routine"] = [dict(routes["tiers"]["routine"][0], model=name)
                                   for name in (model, other)]
    with patch.object(panel_routing, "policy", return_value=routes):
        both = {"pi": {"cooldown": {model: five_min_ago, other: five_min_ago}}}
        assert allocate("codex", installed={"pi"}, provider_status=both)["provider"] == "pi"
        assert allocate("codex", installed={"pi"}, provider_status=cooling)["model"] == other

    # An older offset timestamp sorts later as text; chronological newest still wins.
    older = (now - timedelta(seconds=600)).astimezone(timezone(timedelta(hours=10))).isoformat()
    rows = [(model, five_min_ago), (model, older), (model, thirty_min_ago)]
    for malformed in ("", "0", "malformed", "2026-02-30T00:00:00Z", 0, None, [],
                      now.replace(tzinfo=None).isoformat(), (now + timedelta(seconds=1)).isoformat()):
        rows.extend([(model, malformed), (other, malformed)])
    expected = {model: five_min_ago}
    assert reading(rows)["pi"]["cooldown"] == expected
    assert reading(list(reversed(rows)))["pi"]["cooldown"] == expected
    assert reading([])["pi"]["cooldown"] == {}
    for age in (0, panel_routing.PI_COOLDOWN_SECONDS - 1, panel_routing.PI_COOLDOWN_SECONDS, 1800):
        at = (now - timedelta(seconds=age)).isoformat()
        readings = reading([(model, at)])
        active = age < panel_routing.PI_COOLDOWN_SECONDS
        assert readings["pi"]["cooldown"] == ({model: at} if active else {})
        assert allocate("codex", installed={"pi", "claude"}, provider_status=readings)["provider"] == (
            "claude" if active else "pi")
    for invalid in (None, "", 0, {}, {"cooldown": None}, {"cooldown": []}, {"cooldown": model}):
        assert allocate("codex", installed={"pi", "claude"}, provider_status={"pi": invalid})["provider"] == "pi"


# Exact tier models keep main ownership and judgment on the native pair.
def exercise_exact_model_routing():
    from panel_routing import valid_route
    import panel_routing
    load_policy = panel_routing.policy
    original = load_policy()
    for role in ("reviewer", "verifier"):
        assert allocate("claude", role=role, installed={"claude", "codex"})["workload"] == "deep"
        assert allocate("claude", role=role, tier="frontier", installed={"claude", "codex"})["workload"] == "frontier"
    assert original["tiers"]["light"][0] == {"provider": "pi", "model": "openrouter/thinkingmachines/inkling:free", "effort": "low", "public_only": True}
    for owner in ("claude", "codex"):
        installed = {"pi", "claude", "codex"}
        assert allocate(owner, role="documenter", installed=installed)["provider"] == "pi"
        researcher = allocate(owner, role="researcher", installed=installed)
        assert researcher["provider"] in {"claude", "codex"} and researcher["workload"] == "light" and researcher["effort"] == "low"
        scholar = allocate(owner, role="scholar", installed=installed)
        assert (scholar["kind"], scholar["workload"], scholar["effort"], scholar["purpose"]) == ("researcher", "deep", "high", "research")
        assert scholar["provider"] in {"claude", "codex"}
        try:
            allocate(owner, role="scholar", access="write", installed=installed)
        except ValueError:
            pass
        else:
            raise AssertionError("scholar write access accepted")
    from datetime import datetime, timezone, timedelta
    model = "openrouter/thinkingmachines/inkling:free"
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    yesterday = (datetime.now(timezone.utc) - timedelta(days=1)).strftime("%Y-%m-%d")
    for day in ("", "0", "malformed", yesterday):
        assert allocate("codex", installed={"pi", "claude"}, provider_status={"pi": {"daily_cap": {model: day}}})["provider"] == "pi"
    ledger = [{"refs": {"provider_error_provider": "pi", "provider_error": "capacity",
                        "provider_daily_model": model, "provider_daily_day": today}}]
    with patch("terminal_panel.events", return_value=json.dumps(ledger)), patch.dict(os.environ, {"OMS_ROOM_ID": ""}):
        readings = dispatch.routing_status(project)
    assert allocate("codex", installed={"pi", "claude"}, provider_status=readings)["provider"] == "claude"
    with patch.object(panel_routing, "datetime") as clock:
        clock.now.return_value = datetime(2026, 10, 28, 23, 59, 59, tzinfo=timezone.utc)
        boundary = {"pi": {"daily_cap": {model: "2026-10-28"}}}
        assert allocate("codex", installed={"pi", "claude"}, provider_status=boundary)["provider"] == "claude"
        clock.now.return_value = datetime(2026, 10, 29, 0, 0, 0, tzinfo=timezone.utc)
        assert allocate("codex", installed={"pi", "claude"}, provider_status=boundary)["provider"] == "pi"
    capped = {"pi": {"daily_cap": {model: today}}}
    assert allocate("codex", installed={"pi", "claude"}, provider_status=capped)["provider"] == "claude"
    assert allocate("codex", installed={"pi", "claude"}, provider_status={"pi": {"daily_cap": {"openrouter/other:free": today}}})["provider"] == "pi"
    try:
        allocate("codex", installed={"pi"}, provider_status=capped)
    except ValueError as error:
        assert "cooldown" in str(error)
    else:
        raise AssertionError("sole capped route was accepted")
    exercise_pi_cooldown_routing()
    # Free routes preserve read order but serve writes only after usable native routes.
    providers = {"pi", "claude", "codex"}
    for claude_used, codex_used, expected in ((10, 60, "claude"), (60, 10, "codex")):
        readings = {"claude": {"weekly_used": claude_used}, "codex": {"weekly_used": codex_used}}
        assert allocate("codex", access="write", installed=providers, provider_status=readings,
                        check_visibility=False)["provider"] == expected
        assert allocate("codex", access="read", installed=providers, provider_status=readings,
                        check_visibility=False)["provider"] == "pi"
    assert allocate("codex", access="write", installed={"pi"}, check_visibility=False)["provider"] == "pi"
    for missing in (None, "", "malformed"):
        readings = {"claude": {"weekly_used": missing}, "codex": {"weekly_used": 0}}
        assert allocate("codex", access="write", installed=providers, provider_status=readings,
                        check_visibility=False)["provider"] == "claude"
    for used in (0, 89, 90):
        readings = {"claude": {"weekly_used": used}, "codex": {"weekly_used": 90}}
        expected = "claude" if used < 90 else "pi"
        assert allocate("codex", access="write", installed=providers, provider_status=readings,
                        check_visibility=False)["provider"] == expected
    for used in (0, 14, 15):
        readings = {"claude": {"weekly_used": 0}, "codex": {"weekly_used": used}}
        expected = "claude" if used == 15 else "codex"
        assert allocate("claude", access="write", installed=providers, provider_status=readings,
                        check_visibility=False)["provider"] == expected
    # Read-only investigate workers must avoid Pi when another provider is available.
    assert allocate("codex", role="worker", access="read", purpose="investigate", installed={"pi", "claude"})["provider"] == "claude"
    assert allocate("codex", role="worker", access="read", purpose="investigate", installed={"pi"})["provider"] == "pi"
    assert allocate("codex", role="worker", access="read", purpose="explain", installed={"pi", "claude"})["provider"] == "pi"
    # (a) Catalog refusal for the only non-Pi candidate keeps pi (degraded beats refused).
    assert allocate("codex", role="worker", access="read", purpose="investigate",
                    installed={"pi", "codex"},
                    catalogs={"codex": ["gpt-7-sol"]})["provider"] == "pi"
    # (b) Weekly quota >=90 for the only non-Pi candidate keeps pi.
    assert allocate("codex", role="worker", access="read", purpose="investigate",
                    installed={"pi", "claude"},
                    provider_status={"claude": {"weekly_used": 95}, "pi": {"weekly_used": 0}})["provider"] == "pi"
    for role in ("reviewer", "verifier", "critic", "security-auditor"):
        for target in ("pi", "opencode"):
            with patch.object(panel, "provider_catalog", return_value=[registered[target]]):
                route = panel.selected_route("codex", role, None, "read", None, target=target, model="p/m")
            assert route["provider"] == target and route["model"] == "p/m", route
    for provider, model in (("pi", "openrouter/thinkingmachines/inkling:free"),
                            ("opencode", "openrouter/nvidia/nemotron-3-ultra-550b-a55b:free")):
        configured = {"provider": provider, "model": model, "effort": "medium"}
        assert valid_route(configured), configured
        if provider == "pi":
            assert original["tiers"]["routine"][0] == dict(configured, public_only=True)
        routes = deepcopy(original)
        routes["tiers"]["routine"][0] = configured
        with patch.object(panel_routing, "policy", return_value=routes), \
                patch.object(panel, "policy", return_value=routes):
            assert valid_route(configured), configured
            for malformed in (None, 0, "", "0", "openrouter/", "/model", "-openrouter/model",
                              "openrouter/model name", "openrouter/model\n", "openrouter/\tmodel", "p/\0m"):
                assert not valid_route(dict(configured, model=malformed)), malformed
            assert valid_route(dict(configured, model="p/m"))
            assert registered[provider]["installed"] and registered[provider]["exact_model_override"]
            if provider == "opencode":
                assert registered[provider]["effort_mechanism"] is None
            assert registered[provider]["access"] == ["read", "write"]
            assert panel.policy()["tiers"]["routine"][0] == configured
            assert set(panel.policy()["main"]) == set(panel.PROVIDERS) == {"claude", "codex"}
            invalid_main = deepcopy(panel.policy())
            invalid_main["main"][provider] = configured
            with patch.object(panel_routing.json, "loads", return_value=invalid_main):
                try:
                    load_policy()
                except ValueError as error:
                    assert "main routes" in str(error), error
                else:
                    raise AssertionError("Exact provider became a main route")
            for catalog in ([], [model], ["other/model", model]):
                assert panel.routable_route_model(provider, model, catalog) == model
            try:
                panel.routable_route_model(provider, model, ["other/model"])
            except ValueError as error:
                assert "catalog" in str(error), error
            else:
                raise AssertionError("an absent exact model passed a populated catalog")
            providers = {provider, "claude", "codex"}
            catalogs = {provider: [model], "claude": [], "codex": []}
            earlier = ["other/model-%d" % n for n in range(20)]
            rows = [dict(provider=name, binary=name, installed=True,
                         models=earlier if name == provider else [],
                         catalog_models=earlier + [model] if name == provider else [])
                    for name in providers]
            with patch.object(panel, "provider_catalog", return_value=rows), \
                    patch("panel_dispatch.route_inputs", return_value=({}, None, None)):
                selected = panel.selected_route("codex", "worker", None, "read", None)
                assert selected["provider"] == provider and selected["model"] == model, selected
            for owner in panel.PROVIDERS:
                for used in (None, 0, 15, 89, 90, 100, "malformed"):
                    readings = {name: {"weekly_used": used} for name in ("claude", "codex")}
                    route = allocate(owner, installed=providers, provider_status=readings,
                                     recent={provider: 1000}, catalogs=catalogs)
                    assert route["provider"] == provider, (owner, used, route)
                assert allocate(owner, installed={provider}, catalogs=catalogs)["provider"] == provider
                assert allocate(owner, installed=providers - {provider})["provider"] != owner
                assert allocate(owner, installed={"codex"})["provider"] == "codex"
                assert allocate(owner, installed={"claude"})["provider"] == "claude"
                assert allocate(owner, installed=providers, catalogs=dict(catalogs, **{provider: ["other/model"]}))["provider"] != provider
                for role in ("reviewer", "verifier", "security-auditor", "critic", "advisor", "architect"):
                    judge = allocate(owner, role=role, tier="routine", installed=providers, writer=provider)
                    assert judge["provider"] in panel.PROVIDERS, judge
            assert allocate("codex", role="reviewer", installed=providers, writer=provider,
                            provider_status={"claude": {"weekly_used": 80}, "codex": {"weekly_used": 10}})["provider"] == "codex"
            with_exact_route_light = deepcopy(panel.policy())
            with_exact_route_light["tiers"]["light"].insert(0, dict(configured, effort="low"))
            with patch.object(panel_routing, "policy", return_value=with_exact_route_light):
                researcher = allocate("codex", role="researcher", installed=providers)
                assert researcher["provider"] == "claude", researcher
                assert panel.alternate_route(researcher, providers)["provider"] == "codex"
            for role in ("reviewer", "advisor"):
                try:
                    allocate("codex", role=role, tier="routine", installed={provider})
                except ValueError:
                    pass
                else:
                    raise AssertionError("Exact provider was allocated as the only judge")
            for installed in (set(), {provider}):
                try:
                    allocate("claude", installed=installed, catalogs=dict(catalogs, **{provider: ["other/model"]}))
                except ValueError:
                    pass
                else:
                    raise AssertionError("no eligible route was accepted")
            exact_route = dict(configured, role="worker", kind="worker", owner="codex", workload="routine",
                               access="read", purpose="explain", binary=provider)
            for effort in (None, "low", "medium", "high"):
                argv = panel.routed_command(panel.ENTRY, project, dict(exact_route, effort=effort), prompt="Inspect source.")
                assert ("--reasoning-effort" in argv) == (provider == "pi" and effort is not None), argv
                assert argv[argv.index("--model") + 1] == model, argv
            assert panel.alternate_route(exact_route, providers)["provider"] == "claude"
            assert panel.alternate_route(exact_route, {provider, "codex"})["provider"] == "codex"
            assert panel.alternate_route(exact_route, {provider}) is None
            judge = dict(exact_route, provider="claude", role="reviewer")
            assert panel.alternate_route(judge, {"claude", provider}) is None
            for used, expected in ((0, "claude"), (89, "claude"), (90, "codex"), (100, "codex"),
                                   (None, "claude"), ("malformed", "claude")):
                assert panel.alternate_route(exact_route, providers, {"claude": {"weekly_used": used}})["provider"] == expected
            for installed, expected in ((providers, "claude"), ({provider, "codex"}, "codex"), ({provider}, None)):
                rows = [dict(provider=name, binary=name, installed=name in installed,
                             exact_model_override=True, access=["read", "write"],
                             catalog_models=catalogs[name], effort_mechanism=None if name == "opencode" else "flag")
                        for name in providers]
                with patch.object(panel, "provider_catalog", side_effect=lambda selection="all": [
                        row for row in rows if row["provider"] in selection.split(",")]), \
                        patch.object(panel, "selected_route", return_value=exact_route), \
                        patch.object(panel, "routing_status", return_value={}), \
                        patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: args[-2]):
                    fallback = panel.dispatch(project, "codex", "worker", None, "read", "explain", prompt="Inspect source.")
                    assert (fallback["provider"] if fallback else None) == expected, fallback
            # The board and discovery keep handling a third tier provider without promoting a main.
            text = render({"repo": {"name": "routing"}, "collection": {"ok": True},
                           "room": {"participants": [dict(participant="oc", provider=provider, model=model,
                                                             role="worker", joined=True)]}}, "codex", width=150, height=40)
            assert isinstance(text, str)

def exercise_antigravity_read_tiers():
    import panel_routing as routing
    configured = routing.policy()
    providers = {"claude", "codex", "antigravity"}
    model = "gemini-4-argon-high"
    catalogs = {"antigravity": [model, "gemini-4.10-argon-high", "gemini-5-argon-low"]}
    exhausted = {name: {"weekly_used": 90} for name in ("claude", "codex")}

    def choose(tier, **options):
        arguments = dict(installed=providers, catalogs=catalogs, resolve_accounts=False)
        arguments.update(options)
        return allocate("claude", tier=tier, **arguments)

    def refused(call):
        try:
            call()
        except ValueError as error:
            assert len(str(error).splitlines()) == 1, error
            return str(error)
        raise AssertionError("ineligible tier route accepted")

    for tier in ("deep", "frontier"):
        entry = next(route for route in configured["tiers"][tier] if route["provider"] == "antigravity")
        assert entry == dict(provider="antigravity", model=model, effort="high", access="read")
        assert routing.valid_route(entry)
        for access in (None, "", 0, [], "malformed", "write"):
            invalid = deepcopy(configured)
            invalid_entry = next(route for route in invalid["tiers"][tier] if route["provider"] == "antigravity")
            if access is None:
                del invalid_entry["access"]
            else:
                invalid_entry["access"] = access
            with patch.object(routing.Path, "read_text", return_value=json.dumps(invalid)):
                assert "invalid panel tier" in refused(routing.policy)
        for native in configured["tiers"][tier][:2]:
            for access in ("read", "write"):
                assert routing.valid_route(dict(native, access=access))
            for access in (None, "", 0, [], "malformed"):
                assert not routing.valid_route(dict(native, access=access))

        # Antigravity's unknown quota cannot disturb native quota and tie-band ordering.
        for used, expected in ((0, "codex"), (14, "codex"), (15, "claude"), (89, "claude"), (90, "claude")):
            readings = {"claude": {"weekly_used": 0}, "codex": {"weekly_used": used}}
            assert choose(tier, provider_status=readings)["provider"] == expected
        for used in (0, 89, 90, 100):
            readings = dict(exhausted, claude={"weekly_used": used})
            assert choose(tier, provider_status=readings)["provider"] == ("claude" if used < 90 else "antigravity")
        for unknown in (None, "", "malformed"):
            readings = dict(exhausted, claude={"weekly_used": unknown})
            assert choose(tier, provider_status=readings)["provider"] == "claude"
        # An unknown-quota route keeps its position unless a known native must precede it.
        reordered = deepcopy(configured)
        reordered["tiers"][tier].insert(0, reordered["tiers"][tier].pop())
        with patch.object(routing, "policy", return_value=reordered):
            assert choose(tier)["provider"] == "antigravity"
            readings = dict(exhausted, claude={"weekly_used": 89})
            assert choose(tier, provider_status=readings)["provider"] == "claude"
            previous = dict(provider="codex", workload=tier, access="read", role="worker")
            assert routing.alternate_route(previous, providers, readings, resolve_accounts=False)["provider"] == "claude"
        assert choose(tier, provider_status=exhausted)["provider"] == "antigravity"
        assert choose(tier, installed={"antigravity"})["provider"] == "antigravity"
        for missing in (None, {}, {"antigravity": []}, {"antigravity": ["gemini-5-argon-low"]}):
            assert choose(tier, catalogs=missing)["provider"] == "codex"
            refused(lambda: choose(tier, installed={"antigravity"}, catalogs=missing))
        for role in routing.WRITERS:
            assert choose(tier, role=role, access="write")["provider"] != "antigravity"
            refused(lambda: choose(tier, role=role, access="write", installed={"antigravity"}))
            refused(lambda: choose(tier, role=role, access="write", provider_status=exhausted))
        for role in ("scholar", "reviewer", "verifier", "security-auditor", "critic", "advisor", "architect"):
            route = choose(tier, role=role, writer="claude", provider_status=exhausted)
            assert route["provider"] == "antigravity" and route["access"] == "read", route
            if role in routing.INDEPENDENT:
                assert choose(tier, role=role, writer="antigravity")["provider"] != "antigravity"
                assert choose(tier, role=role, installed={"claude", "antigravity"}, writer="claude",
                              provider_status={"claude": {"weekly_used": 0}})["provider"] == "antigravity"
                with patch.object(routing, "model_family", return_value="google"):
                    assert allocate("antigravity", role=role, tier=tier, installed=providers,
                                    catalogs=catalogs, owner_model=model)["provider"] != "antigravity"
            argv = panel.routed_command(panel.ENTRY, project, route, prompt="Inspect source.",
                                        verify="true" if role == "reviewer" else None)
            assert "antigravity" in " ".join(argv), argv
            if role not in ("reviewer", "advisor"):
                assert "--read-only" in argv, argv
        native = choose(tier, installed={"claude"})
        assert routing.alternate_route(native, {"antigravity"})["provider"] == "antigravity"
        assert routing.alternate_route(dict(native, access="write"), {"antigravity"}) is None
        restricted = deepcopy(configured)
        restricted["tiers"][tier][0]["access"] = "read"
        with patch.object(routing, "policy", return_value=restricted):
            assert choose(tier, access="write")["provider"] == "codex"
            assert routing.alternate_route(dict(native, provider="codex", access="write"), {"claude"}) is None

    rows = [dict(provider=name, binary=name, present=True, installed=True, exact_model_override=True,
                 access=["read", "write"],
                 catalog_models=catalogs.get(name, []), routable=catalogs.get(name, [])) for name in providers]
    for available in (None, 0, "malformed", [], ["gemini-5-argon-low"], catalogs["antigravity"]):
        raw = [dict(row) for row in rows]
        agy = next(row for row in raw if row["provider"] == "antigravity")
        agy["catalog_models"] = catalogs["antigravity"] + ["gemini-99-argon-high"]
        if available is None:
            del agy["routable"]
        else:
            agy["routable"] = available
        def registry_read(arguments, **kwargs):
            if arguments[2] == "models":
                selection = arguments[arguments.index("--providers") + 1]
                shown = [row for row in raw if selection == "all" or row["provider"] in selection.split(",")]
                return subprocess.CompletedProcess(arguments, 0, json.dumps({"providers": shown}), "")
            assert arguments[3] == "panel-provider-access", arguments
            return subprocess.CompletedProcess(arguments, 0, "".join(name + "\ttrue\n" for name in providers), "")

        with patch.object(dispatch.subprocess, "run", side_effect=registry_read), \
                patch.object(panel, "native_adapter", return_value=None), \
                patch.object(dispatch, "route_inputs", return_value=(exhausted, None, None)), \
                patch.object(panel, "route_inputs", return_value=(exhausted, None, None)):
            observed = next(row for row in panel.provider_catalog() if row["provider"] == "antigravity")
            assert observed["catalog_models"] == agy["catalog_models"]
            output = io.StringIO()
            with redirect_stdout(output):
                assert panel.main(["--providers", "--json"]) == 0
            displayed = next(row for row in json.loads(output.getvalue())["providers"]
                             if row["provider"] == "antigravity")
            assert displayed["catalog_models"] == agy["catalog_models"]
            output = io.StringIO()
            with redirect_stdout(output), patch.object(panel, "caller_owner", return_value="claude"):
                assert panel.main(["--routes"]) == 0
            chosen = json.loads(output.getvalue())["chosen"]["scholar"]
            if available == catalogs["antigravity"]:
                selected = panel.selected_route("claude", "scholar", "deep", "read", None)
                assert selected["model"] == "gemini-4.10-argon-high", selected
                assert chosen == {
                    "provider": "antigravity", "model": selected["model"], "effort": "high"}
            else:
                assert "error" in chosen, chosen
                refused(lambda: panel.selected_route("claude", "scholar", "deep", "read", None))
            primary = dict(configured["tiers"]["deep"][0], role="worker", kind="worker", owner="claude",
                           workload="deep", access="read", purpose="explain")
            for exact in (False, True):
                agy["exact_model_override"] = exact
                with patch.object(panel, "selected_route", return_value=primary), \
                        patch.object(panel, "child_environment", return_value={}), \
                        patch.object(dispatch.room, "resolve", return_value=None), \
                        patch.object(panel, "routing_status", return_value=exhausted), \
                        patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: (
                            args[-2], kwargs["fallback_decision"])):
                    fallback, decision = panel.dispatch(project, "claude", "worker", "deep", "read", "explain",
                                                        prompt="Inspect source.")
                if exact and available == catalogs["antigravity"]:
                    assert fallback["provider"] == "antigravity" and fallback["model"] == "gemini-4.10-argon-high"
                else:
                    assert fallback is None and decision == "alternate-not-routable", (fallback, decision)

# BEGIN tier routing
with patch.dict(os.environ, {"OMS_PANEL_PUBLIC_ROUTE_OPT_IN": "1"}):
    exercise_exact_model_routing()
exercise_antigravity_read_tiers()
# END tier routing

# Third-party tier routing requires a public origin or an explicit opt-in.
def exercise_public_only_routing():
    import panel_routing
    import subprocess
    repo = temporary / "public-route-repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "remote", "add", "origin", "https://github.com/example/sample.git"], check=True)
    binaries = temporary / "public-route-bin"
    binaries.mkdir()
    gh = binaries / "gh"
    calls = temporary / "visibility-calls"
    gh.write_text("#!/bin/sh\nprintf x >> \"$ROUTE_CALLS\"\nprintf '{\"visibility\":\"%s\"}' \"$ROUTE_VISIBILITY\"\n")
    gh.chmod(0o755)
    env = {"PATH": str(binaries) + os.pathsep + os.environ["PATH"], "ROUTE_CALLS": str(calls),
           "ROUTE_VISIBILITY": "PUBLIC", "OMS_PANEL_PUBLIC_ROUTE_OPT_IN": ""}
    cache = repo / ".oms/panel-state/public-route-visibility.json"
    with patch.dict(os.environ, env):
        chosen = allocate("codex", installed={"pi", "claude"}, repo=repo)
        assert chosen["provider"] == "pi", chosen
        assert calls.read_text() == "x" and not cache.exists(), "a public answer was cached"
        # A repository made private loses free routes on the next check.
        os.environ["ROUTE_VISIBILITY"] = "PRIVATE"
        assert allocate("codex", installed={"pi", "claude"}, repo=repo)["provider"] == "claude"
        assert calls.read_text() == "xx"
        saved_cache = json.loads(cache.read_text())
        assert saved_cache["public"] is False
        now = saved_cache["checked_at"]
        os.environ["ROUTE_VISIBILITY"] = "PUBLIC"
        for age in (0, 599):
            with patch.object(panel_routing.time, "time", return_value=now + age):
                assert not panel_routing.repository_public(repo)
        assert calls.read_text() == "xx"
        with patch.object(panel_routing.time, "time", return_value=now + 600):
            assert panel_routing.repository_public(repo)
        assert calls.read_text() == "xxx"
        os.environ["ROUTE_VISIBILITY"] = "PRIVATE"
        cache.write_text(json.dumps(dict(saved_cache, public=True)))
        assert not panel_routing.repository_public(repo), "a stored public answer was trusted"
        for bad in ("", "0", "malformed"):
            cache.write_text(bad)
            assert not panel_routing.repository_public(repo)
        cache.unlink()
        notice = io.StringIO()
        with redirect_stderr(notice):
            chosen = allocate("codex", installed={"pi", "claude"}, repo=repo)
            assert chosen["provider"] == "claude", chosen
            assert panel_routing.alternate_route(chosen, {"pi"}, repo=repo) is None
            try:
                allocate("codex", installed={"pi"}, repo=repo)
            except ValueError:
                pass
            else:
                raise AssertionError("private repository received its sole public-only route")
        assert not notice.getvalue(), "public-only filtering wrote to stderr"
        os.environ["OMS_PANEL_PUBLIC_ROUTE_OPT_IN"] = "1"
        assert allocate("codex", installed={"pi", "claude"}, repo=repo)["provider"] == "pi"
        assert panel_routing.alternate_route(chosen, {"pi"}, repo=repo)["provider"] == "pi"
        os.environ["OMS_PANEL_PUBLIC_ROUTE_OPT_IN"] = ""
        cache.unlink()
        real_run = subprocess.run
        for failure in (FileNotFoundError(), subprocess.TimeoutExpired("gh", 5)):
            def failing_gh(args, **kwargs):
                if args[0] == "gh":
                    raise failure
                return real_run(args, **kwargs)
            with patch.object(panel_routing.subprocess, "run", side_effect=failing_gh):
                assert not panel_routing.repository_public(repo)
            negative = json.loads(cache.read_text())
            assert negative["public"] is False
            with patch.object(panel_routing.time, "time", return_value=negative["checked_at"] + 599):
                assert not panel_routing.repository_public(repo)
            os.environ["ROUTE_VISIBILITY"] = "PUBLIC"
            with patch.object(panel_routing.time, "time", return_value=negative["checked_at"] + 600):
                assert panel_routing.repository_public(repo)
            cache.unlink()
        for bad in ("", "0", "invalid"):
            os.environ["OMS_PANEL_PUBLIC_ROUTE_OPT_IN"] = bad
            assert not panel_routing.repository_public(None)
        sample_value = "fixture-" + "pass" + "word"
        remote = "https://user:" + sample_value + "@github.com/example/sample.git"
        subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin", remote], check=True)
        assert not panel_routing.repository_public(repo)
        assert not cache.exists() or sample_value not in cache.read_text(), "rejected origin was cached"
        if cache.exists():
            cache.unlink()
        subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin", "https://example.org/owner/repo"], check=True)
        assert not panel_routing.repository_public(repo)
        assert not cache.exists(), "non-GitHub origin was cached"
        assert not panel_routing.valid_route({"provider": "pi", "model": "p/m", "effort": "low", "public_only": 0})
        assert not panel_routing.valid_route({"provider": "pi", "model": "p/m", "effort": "low", "unexpected": True})
    # A public cache written in a work repo must not authorize a private state repo.
    work_repo = temporary / "public-route-work"
    work_repo.mkdir()
    subprocess.run(["git", "init", "-q", str(work_repo)], check=True)
    subprocess.run(["git", "-C", str(work_repo), "remote", "add", "origin", "https://github.com/example/sample.git"], check=True)
    work_cache = work_repo / ".oms/panel-state/public-route-visibility.json"
    work_cache.parent.mkdir(parents=True, exist_ok=True)
    work_cache.write_text(json.dumps({"origin": "example/sample", "checked_at": time.time(), "public": True}))
    # The authoritative (state) repo is private; visibility resolves against it.
    state_repo = temporary / "public-route-state"
    state_repo.mkdir()
    subprocess.run(["git", "init", "-q", str(state_repo)], check=True)
    subprocess.run(["git", "-C", str(state_repo), "remote", "add", "origin", "https://github.com/example/private.git"], check=True)
    state_cache = state_repo / ".oms/panel-state/public-route-visibility.json"
    state_cache.parent.mkdir(parents=True, exist_ok=True)
    state_cache.write_text(json.dumps({"origin": "example/private", "checked_at": time.time(), "public": False}))
    route = dict(provider="claude", model="claude-sonnet-5-5", effort="medium", role="worker",
                 kind="worker", owner="codex", workload="routine", access="read", purpose="explain")
    rows = [dict(provider=name, binary=name, installed=True, exact_model_override=True,
                 access=["read", "write"], catalog_models=[]) for name in ("pi", "claude")]
    def route_catalog(selection="all"):
        return [row for row in rows if selection == "all" or row["provider"] in selection.split(",")]
    cache.unlink(missing_ok=True)
    calls.unlink(missing_ok=True)
    subprocess.run(["git", "-C", str(repo), "remote", "set-url", "origin", "https://github.com/example/sample.git"], check=True)
    notice = io.StringIO()
    with patch.dict(os.environ, env), patch.object(panel, "provider_catalog", side_effect=route_catalog), \
            redirect_stderr(notice):
        chosen = panel.selected_route("codex", "worker", None, "read", "explain", target="pi", model="p/m", repo=repo)
        assert chosen["provider"] == "pi" and chosen["model"] == "p/m"
        assert not calls.exists() or not calls.read_text(), "explicit model checked visibility"
        assert not cache.exists(), "explicit model wrote visibility cache"
        os.environ["ROUTE_VISIBILITY"] = "PRIVATE"
        try:
            panel.selected_route("codex", "worker", None, "read", "explain", target="pi", repo=repo)
        except ValueError as error:
            assert "requires an explicit valid --model" in str(error)
        else:
            raise AssertionError("private tier route was accepted")
        assert calls.read_text() == "x"
        assert not notice.getvalue(), "public-only filtering wrote to stderr"
    with patch.dict(os.environ, {"OMS_PANEL_PUBLIC_ROUTE_OPT_IN": ""}), \
            patch.object(panel, "provider_catalog", side_effect=route_catalog), \
            patch.object(dispatch, "route_inputs", return_value=({}, None, None)):
        chosen = panel.selected_route("codex", "worker", None, "read", "explain", repo=state_repo)
        assert chosen["provider"] == "claude", chosen
    with patch.dict(os.environ, {"OMS_PANEL_PUBLIC_ROUTE_OPT_IN": ""}), \
            patch.object(panel, "selected_route", return_value=route), \
            patch.object(panel, "routing_status", return_value={}), \
            patch.object(panel, "provider_catalog", side_effect=route_catalog), \
            patch.object(panel, "alternate_route", wraps=panel.alternate_route) as alternate, \
            patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: args[-2]):
        fallback = panel.dispatch(state_repo, "codex", "worker", None, "read", "explain",
                                  prompt="Inspect source.", work_repo=work_repo)
        assert fallback is None, fallback
        assert alternate.call_args.kwargs["repo"] == state_repo

exercise_public_only_routing()

# BEGIN settled overlap warning
# A finished write call keeps its declared scope in the advisory while its patch awaits a decision.
finished_worker = {"participant": "W1", "joined": True, "parent": "main", "role": "worker",
                   "owns": ["src/shared.py"]}
finished_room = {"participants": [finished_worker]}
finished_scope_state = {
    "call_results": {"W1": {"task_id": "finished-patch-task", "patch": ".oms/artifacts/patches/W1.patch",
                             "needs_review": True}},
    "room_attempts": {"W1": {"task_id": "finished-patch-task", "state": "review"}},
    "finalized": {},
}
with patch.object(panel, "_dispatch_call_finished", return_value=True), \
        patch.object(panel, "_dispatch_call_record", side_effect=AssertionError("unexpected call-record lookup")), \
        patch.object(panel, "results", side_effect=AssertionError("unexpected full result lookup")):
    overlap_room = dict(finished_room, scope_review_state=finished_scope_state)
    overlap = dispatch.scope_overlaps(temporary, "room", overlap_room, "main", ["src/shared.py"])
    assert overlap == [{"participant": "W1", "role": "worker", "scope": "src/shared.py",
                        "status": "finished, patch awaiting decision"}], overlap
    finalized_scope_state = dict(finished_scope_state, finalized={"finished-patch-task": "rejected"})
    finalized_room = dict(finished_room, scope_review_state=finalized_scope_state)
    assert dispatch.scope_overlaps(temporary, "room", finalized_room, "main", ["src/shared.py"]) == []
    assert dispatch.scope_overlaps(temporary, "room", finished_room, "main", ["src/shared.py"]) == []

    report = {"room": dict(finished_room, id="room", call_results=finished_scope_state["call_results"]),
              "call_results": finished_scope_state["call_results"],
              "room_attempts": finished_scope_state["room_attempts"], "finalized": {},
              "awaiting_admission": ["W1"]}
    def warnings(snapshot):
        return dispatch.scope_overlaps(temporary, "room", dict(finished_room, scope_review_state=snapshot),
                                       "main", ["src/shared.py"])
    assert warnings(report)
    assert not warnings(dict(report, finalized={"finished-patch-task": "admitted"}))
    assert not warnings(dict(report, plan={"tasks": [{"id": "finished-patch-task", "state": "done"}]}))
    later = dict(finished_worker, participant="W2", owns=[], seq=2)
    newer = dict(report, room=dict(report["room"], participants=[dict(finished_worker, seq=1), later]),
                 room_attempts=dict(report["room_attempts"], W2={"task_id": "finished-patch-task", "state": "review"}))
    assert not warnings(newer)
    assert warnings(dict(report, finalized={"finished-patch-task": "failed"}))
    done = dict(report, room_attempts={"W1": {"task_id": "finished-patch-task", "state": "done"}})
    assert warnings(done)
    assert not warnings(dict(done, awaiting_admission=[]))
    attempt = {"attempt_id": "att-w1", "task_id": "finished-patch-task", "state": "review", "refs": {}}
    patch_row = {"kind": "delegate", "attempt_id": "att-w1", "task_id": "finished-patch-task",
                 "patch": "w.patch", "patch_sha256": "a" * 64}
    recorded = dict(report, room_attempts={"W1": attempt}, admission_records=([attempt], [patch_row]))
    assert warnings(recorded)
    for kind in ("patch-admit", "patch-land"):
        receipt = {"kind": kind, "task_id": "finished-patch-task", "patch_sha256": "a" * 64}
        assert not warnings(dict(recorded, admission_records=([attempt], [patch_row, receipt])))
    owner = {"participant": "main", "joined": True, "role": "main", "seq": 0}
    exited = dict(report, room=dict(report["room"], participants=[owner, finished_worker]),
                  room_attempts=dict(report["room_attempts"], main={"state": "done"}))
    assert not warnings(exited)

    route = {"kind": "worker", "provider": "codex", "model": "test", "role": "worker",
             "purpose": "implement", "workload": "routine"}
    main_record = {"attempt_id": "main", "tool": "panel-main", "provider": "codex",
                   "terminal": False, "refs": {"panel_room_participant": "main", "panel_room_id": "room"}}
    with patch.dict(os.environ, {"OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}), \
            patch.object(panel, "_validate_dispatch_verifier"), patch.object(panel, "_validate_dry_run_inputs"), \
            patch.object(panel, "routed_command", return_value=["delegate", "--repo", str(root),
                                                                "--verify", "true", "--task-id", "plan-task"]), \
            patch.object(panel, "child_environment", return_value={"OMS_ROOM_ID": "room",
                         "OMS_ROOM_PARTICIPANT": "main", "OMS_PANEL_MAIN_ATTEMPT": "main"}), \
            patch.object(panel, "events", return_value=json.dumps(main_record)), \
            patch.object(panel, "active_mains", return_value={"main": main_record}), \
            patch.object(dispatch.room, "resolve", return_value="room"), \
            patch.object(dispatch.room, "records", return_value=[]), \
            patch.object(dispatch.room, "project", return_value=finished_room), \
            patch.object(dispatch.room, "participant", return_value={"role": "main", "provider": "codex"}), \
            patch.object(dispatch.room, "same_room", return_value=True), \
            patch.object(dispatch, "_plan_task_paths", return_value=(["./src/shared.py"], [])), \
            patch.object(panel, "scope_overlaps", return_value=overlap) as overlap_check, \
            redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
        def preview(role, access, scopes):
            return dispatch.run_dispatch(root, "codex", role, access, None, None, "true", "plan-task",
                                         True, None, dict(route, role=role), "main", None, scopes, work_repo=root)
        assert preview("worker", "write", None) == 0
        assert overlap_check.call_args.args[-1] == ["src/shared.py"]
        assert preview("worker", "write", ["extra.py"]) == 0
        assert overlap_check.call_args.args[-1] == ["extra.py", "src/shared.py"]
        overlap_check.reset_mock()
        assert preview("researcher", "read", None) == 0
        assert not overlap_check.called
        with patch.object(dispatch, "_plan_task_paths", return_value=(["../bad*"], [])):
            assert preview("worker", "write", None) == 0
            assert not overlap_check.called
        def check_declaration():
            for paths, expected in (([], []), (["p%d" % n for n in range(16)], ["p%d" % n for n in range(16)]),
                                    (["p%d" % n for n in range(17)], []), (["../bad*"], [])):
                with patch.object(dispatch, "_plan_task_paths", return_value=(paths, [])):
                    assert dispatch._owned_scopes(root, "plan-task", "worker", None) == expected
            with patch.object(dispatch.shutil, "which", return_value="delegate"), \
                    patch.object(panel, "work_repository", return_value=root), \
                    patch.object(panel, "snapshot", side_effect=ValueError("no snapshot")), \
                    patch.object(panel, "_rollover_if_needed", return_value=None), \
                    patch.object(panel, "_dispatch_capacity"), patch.object(panel, "_reconcile_dispatch"), \
                    patch.object(dispatch.subprocess, "run", return_value=Mock(returncode=0)), \
                    patch.object(panel, "plan_link", return_value=None), \
                    patch.object(dispatch, "_reserve_alias", return_value="worker"), \
                    patch.object(dispatch.room, "join", side_effect=RuntimeError("joined")) as join:
                with patch.object(panel, "events", side_effect=[json.dumps(main_record), "[]"]):
                    try:
                        dispatch.run_dispatch(root, "codex", "worker", "write", None, None, "true", "plan-task",
                                              False, None, route, "main", None, None, work_repo=root)
                        raise AssertionError("join was not reached")
                    except RuntimeError as error:
                        assert str(error) == "joined"
                declared = join.call_args.kwargs["owns"]
                assert declared == ["src/shared.py"]
                assert warnings(dict(report, room=dict(report["room"], participants=[dict(finished_worker, owns=declared)])))
        check_declaration()
# END settled overlap warning

# BEGIN continuation admission hint
digest_a, digest_b = "a" * 64, "b" * 64
workers = [{"task_id": "chain", "attempt_id": "a", "refs": {}},
           {"task_id": "chain", "attempt_id": "b", "refs": {"panel_continues": "a"}}]
records = [{"task_id": "chain", "kind": "delegate", "attempt_id": "a", "patch": "a.patch", "patch_sha256": digest_a},
           {"task_id": "chain", "kind": "delegate", "attempt_id": "b", "patch": "b.patch", "patch_sha256": digest_b},
           {"task_id": "chain", "kind": "patch-land", "patch_sha256": digest_b}]
for receipt in (records[-1], dict(records[-1], kind="patch-admit"),
                dict(records[-1], kind="panel-accept", attempt_id="b")):
    assert saved._scan_admissions(workers, records[:2] + [receipt], {"chain"})[0] == set()
for ref in (None, "", "missing", 0):
    unrelated = [workers[0], dict(workers[1], refs={"panel_continues": ref})]
    assert saved._scan_admissions(unrelated, records, {"chain"})[0] == {"chain"}
assert saved._scan_admissions(workers, records[:2], {"chain"})[0] == {"chain"}
# END continuation admission hint

def exercise_continuation_preference():
    from panel_routing import allocate
    status = {"codex": {"weekly_used": 80}, "claude": {"weekly_used": 10}}
    args = dict(owner="codex", role="builder", tier="routine", access="write",
                installed={"codex", "claude"}, provider_status=status, check_visibility=False)
    assert allocate(**args, preferred="codex")["provider"] == "codex"
    assert allocate(**dict(args, installed={"claude"}), preferred="codex")["provider"] == "claude"
    assert allocate(**args, preferred="missing")["provider"] == "claude"
    for preferred in (None, "", 0, "malformed"):
        assert allocate(**args, preferred=preferred)["provider"] == "claude"
    for usage, expected in ((0, "codex"), (89, "codex"), (90, "claude")):
        readings = dict(status, codex={"weekly_used": usage})
        assert allocate(**dict(args, provider_status=readings), preferred="codex")["provider"] == expected
    from datetime import datetime, timezone
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    import panel_routing
    model = next(c["model"] for c in panel_routing.policy()["tiers"]["routine"] if c["provider"] == "codex")
    capped = dict(status, codex={"daily_cap": {model: today}})
    assert allocate(**dict(args, provider_status=capped), preferred="codex")["provider"] == "claude"
    assert allocate(**dict(args, role="reviewer", access="read"), preferred="codex")["provider"] == "claude"


exercise_continuation_preference()

# BEGIN continuation route provenance
with patch.object(panel, "child_environment", return_value={}), \
     patch.object(dispatch.room, "resolve", return_value="room"), \
     patch.object(panel, "routing_status", return_value={}), \
     patch.object(panel, "resolve_call_alias", return_value="task"), \
     patch.object(panel, "owning_attempt", return_value=["parent"]), \
     patch.object(panel, "events", return_value=json.dumps([
         {"attempt_id": "parent", "refs": {"panel_room_participant": "owner"}},
         {"attempt_id": "attempt", "parent_attempt_id": "parent", "task_id": "task",
          "provider": "codex", "refs": {"panel_role": "worker", "panel_room_id": "room",
          "panel_room_participant": "member", "panel_model": "gpt-6.1-sol", "panel_effort": "high"}}])), \
     patch.object(panel, "_dispatch_call_record", return_value={"schema": 2, "task_id": "task"}), \
     patch.object(dispatch.dispatch_store, "read") as anchor, \
     patch.object(panel, "selected_route", side_effect=RuntimeError("captured")) as selected:
    for flags, overrides, expected in (
            ({"model_explicit": True, "effort_explicit": True}, {}, (None, "codex", "gpt-6.1-sol", "high")),
            ({"model_explicit": False, "effort_explicit": False}, {}, ("routine", None, None, None)),
            ({}, {}, ("routine", None, None, None)),
            ({"model_explicit": True, "effort_explicit": True},
             {"target": "claude", "model": "claude-test", "effort": "low"},
             ("routine", "claude", "claude-test", "low")),
            ({"model_explicit": False, "effort_explicit": True}, {}, ("routine", None, None, "high")),
            ({"model_explicit": True, "effort_explicit": True}, {"effort": "low"},
             (None, "codex", "gpt-6.1-sol", "low")),
            ({"model_explicit": True, "effort_explicit": False}, {"target": "claude"},
             ("routine", "claude", None, None)),
            ({"model_explicit": "true", "effort_explicit": 0}, {}, ("routine", None, None, None))):
        anchor.return_value = {"route": flags}
        try:
            dispatch.dispatch(temporary, "codex", "builder", "routine", "write", None,
                              brief="brief", continue_task="task", **overrides)
        except RuntimeError as error:
            assert str(error) == "captured"
        args = selected.call_args.args
        assert (args[2], args[5], args[6], args[7]) == expected, args
    anchor.return_value = {"route": {"model_explicit": True, "account": "fixture", "account_reason": "selected"}}
    selected.side_effect = ValueError("retired model")
    try:
        dispatch.dispatch(temporary, "codex", "builder", "routine", "write", None,
                          brief="brief", continue_task="task")
        raise AssertionError("invalid inherited model was accepted")
    except ValueError as error:
        assert all(part in str(error) for part in ("gpt-6.1-sol", "attempt", "--to", "--model", "--workload", "retired model")), error
    try:
        dispatch.dispatch(temporary, "codex", "builder", "routine", "write", None,
                          brief="brief", continue_task="task", workload_explicit=True)
        raise AssertionError("invalid selected tier was accepted")
    except ValueError as error:
        assert str(error) == "retired model", error
with patch.object(dispatch.dispatch_store, "initialize"), \
     patch.object(dispatch.dispatch_store, "generation", return_value="generation"), \
     patch.object(dispatch.dispatch_store, "write") as write, \
     patch.dict(panel.DURABLE, {"atomic_mutate": lambda *args, **kwargs: None}):
    flags = {"model_explicit": True, "effort_explicit": False, "account": "fixture", "account_reason": "selected"}
    dispatch._prepare_dispatch_call(temporary, "room", "call-member", "owner", route_provenance=flags)
    assert write.call_args.args[2]["route"] == flags
    assert write.call_args.kwargs["exclusive"] is True
# The public CLI must retain whether workload came from the user.
cli_brief = temporary / "continuation-route-brief.md"
cli_brief.write_text("Continue.\n")
def cli_route(*args, **kwargs):
    if kwargs.get("routing_readings") is not None:
        raise RuntimeError(repr((args[2], args[5], args[6])))
    return {"provider": "codex", "model": "fixture", "effort": "medium", "workload": "routine"}
with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "0", "OMS_HARNESS_DELEGATE_DEPTH": "0"}), \
     patch.object(panel, "continuation_defaults", return_value={"role": "builder", "scope": [],
                  "workload": "routine", "verify": None, "purpose": "implement"}), \
     patch.object(panel, "panel_repository", return_value=temporary), \
     patch.object(panel, "git_toplevel", return_value=temporary), \
     patch.object(panel, "work_repository", return_value=temporary), \
     patch.object(panel, "routed_command", return_value=[]), \
     patch.object(panel, "child_environment", return_value={}), \
     patch.object(dispatch.room, "resolve", return_value="room"), \
     patch.object(panel, "routing_status", return_value={}), \
     patch.object(panel, "resolve_call_alias", return_value="task"), \
     patch.object(panel, "owning_attempt", return_value=["parent"]), \
     patch.object(panel, "events", return_value=json.dumps([
         {"attempt_id": "parent", "refs": {"panel_room_participant": "owner"}},
         {"attempt_id": "attempt", "parent_attempt_id": "parent", "task_id": "task",
          "provider": "codex", "refs": {"panel_role": "worker", "panel_room_id": "room",
          "panel_room_participant": "member", "panel_model": "gpt-6.1-sol"}}])), \
     patch.object(panel, "_dispatch_call_record", return_value={"schema": 2, "task_id": "task"}), \
     patch.object(dispatch.dispatch_store, "read", return_value={"route": {"model_explicit": True}}), \
     patch.object(panel, "selected_route", side_effect=cli_route):
    try:
        panel.main(["--repo", str(temporary), "--owner", "codex", "--continue", "task",
                    "--brief-file", str(cli_brief), "--workload", "deep"])
        raise AssertionError("dispatch did not select a route")
    except RuntimeError as error:
        assert str(error) == repr(("deep", None, None)), error
# END continuation route provenance

# Dispatch passes the actual provider of the newest owned writer, including alias lookup.
def exercise_continuation_dispatch_preference():
    rows = [{"attempt_id": "one", "parent_attempt_id": "parent", "task_id": "task",
             "created_at": "1", "provider": "claude", "refs": {"panel_role": "worker"}},
            {"attempt_id": "two", "parent_attempt_id": "parent", "task_id": "task",
             "created_at": "2", "provider": "claude",
             "refs": {"panel_role": "worker", "panel_provider": "codex"}}]
    captured = []
    def capture(*args, **kwargs):
        preferred = kwargs.get("preferred")
        route = allocate("codex", role="builder", tier="routine", access="write",
                         installed={"codex", "claude"}, check_visibility=False,
                         provider_status={"codex": {"weekly_used": 80}, "claude": {"weekly_used": 10}},
                         preferred=preferred)
        captured.append((preferred, route["provider"]))
        raise RuntimeError("route captured")
    with patch.object(panel, "child_environment", return_value={}), \
         patch.object(dispatch.room, "resolve", return_value="room"), \
         patch.object(panel, "routing_status", return_value={}), \
         patch.object(panel, "resolve_call_alias", return_value="task"), \
         patch.object(panel, "owning_attempt", return_value=["parent"]), \
         patch.object(panel, "events", return_value=json.dumps(rows)), \
         patch.object(panel, "selected_route", side_effect=capture):
        for target in (None, "claude"):
            try:
                dispatch.dispatch(temporary, "codex", "builder", "routine", "write", None,
                                  brief="brief", continue_task="alias", target=target)
            except RuntimeError as error:
                assert str(error) == "route captured"
    assert captured == [("codex", "codex"), (None, "claude")], captured

exercise_continuation_dispatch_preference()

# Continuation checks the execution worktree's clean HEAD, not room metadata or dirty files.
continuation_patch = b"--- a/shared.txt\n+++ b/shared.txt\n@@ -1 +1 @@\n-before\n+worker change\n"

def continuation_git_repo(name):
    repo = temporary / name
    repo.mkdir()
    subprocess.run(["git", "-C", str(repo), "init", "-q"], check=True)
    for key, value in (("user.name", "continuation fixture"), ("user.email", "continuation@example.com")):
        subprocess.run(["git", "-C", str(repo), "config", key, value], check=True)
    (repo / "shared.txt").write_text("before\n")
    subprocess.run(["git", "-C", str(repo), "add", "shared.txt"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
    base = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    return repo, base

def run_continuation_fixture(room_repo, work_repo, base, answer=None):
    import panel_results as continuation_results
    call = {"attempt_id": "worker-attempt", "exit": 0, "patch": "patches/continued.patch",
            "patch_sha256": "fixture-sha", "event_id": "fixture-event"}
    if answer is not None:
        call["answer"] = answer
    previous = {"attempt_id": "worker-attempt", "parent_attempt_id": "parent", "terminal": True,
                "refs": {"panel_access": "write", "panel_location": "repository", "panel_provider": "codex"}}
    rows = [{"attempt_id": "parent", "terminal": False}, previous]
    artifact = {"attempt_id": "worker-attempt", "kind": "delegate", "patch": call["patch"],
                "patch_sha256": call["patch_sha256"], "event_id": call["event_id"], "base_sha": base}
    brief = temporary / "continuation-regression-brief.md"
    brief.write_text("Continue the task.\n")
    reference = {}
    with patch.object(panel, "work_repository", side_effect=lambda path: Path(path)), \
         patch.object(panel, "owning_attempt", return_value=["parent"]), \
         patch.object(panel, "events", return_value=json.dumps(rows)), \
         patch.object(panel, "_rework_history", return_value=[previous]), \
         patch.object(panel, "results", return_value={"rows": [{"calls": [call]}]}), \
         patch.object(panel, "session_stored", return_value=False), \
         patch.object(panel, "verified_patch", return_value=continuation_patch), \
         patch.object(continuation_results, "_records", return_value=([], [artifact])):
        materialized, _, note = dispatch.continuation(
            room_repo, "codex", None, "continuation-regression", {"provider": "codex"}, brief, {},
            work_repo=work_repo, patch_reference=reference)
    if reference:
        assert reference["attempt"] == previous["attempt_id"], reference
    text = Path(materialized).read_text()
    Path(materialized).unlink()
    return note, text

# The room repository remains at the old base while the actual dispatch worktree conflicts.
room_repo, continuation_base = continuation_git_repo("continuation-room-repo")
work_repo = temporary / "continuation-work-repo"
subprocess.run(["git", "clone", "-q", str(room_repo), str(work_repo)], check=True)
(work_repo / "shared.txt").write_text("conflicting change\n")
subprocess.run(["git", "-c", "user.name=continuation fixture", "-c", "user.email=continuation@example.com", "-C", str(work_repo), "commit", "-qam", "conflicting drift"], check=True)
assert subprocess.check_output(["git", "-C", str(room_repo), "rev-parse", "HEAD"], text=True).strip() == continuation_base
try:
    run_continuation_fixture(room_repo, work_repo, continuation_base)
    raise AssertionError("conflicting worktree continuation was accepted")
except ValueError as error:
    assert "shared.txt" in str(error) and continuation_base in str(error), error

# Dirty working-tree content must not affect an applicability check against a clean HEAD.
dirty_repo, dirty_base = continuation_git_repo("continuation-dirty-repo")
(dirty_repo / "other.txt").write_text("unrelated\n")
subprocess.run(["git", "-C", str(dirty_repo), "add", "other.txt"], check=True)
subprocess.run(["git", "-c", "user.name=continuation fixture", "-c", "user.email=continuation@example.com", "-C", str(dirty_repo), "commit", "-qm", "unrelated drift"], check=True)
(dirty_repo / "shared.txt").write_text("dirty conflict\n")
dirty_note, _ = run_continuation_fixture(dirty_repo, dirty_repo, dirty_base)
assert "differs from current HEAD" in dirty_note and "applies cleanly" in dirty_note, dirty_note

flagged_answer = "pass" + "word" + "=" + "fixture-value"
flagged_note, flagged_text = run_continuation_fixture(dirty_repo, dirty_repo, dirty_base, flagged_answer)
assert "earlier summary omitted: flagged by the outbound scan" in flagged_text, flagged_text
assert flagged_answer not in flagged_text and "summary omitted" in flagged_note

# An abbreviated recorded base that resolves to HEAD is not drift.
abbrev_repo, abbrev_base = continuation_git_repo("continuation-abbreviated-base-repo")
exact_note, _ = run_continuation_fixture(abbrev_repo, abbrev_repo, abbrev_base)
abbrev_note, _ = run_continuation_fixture(abbrev_repo, abbrev_repo, abbrev_base[:7])
assert "differs from current HEAD" not in exact_note, exact_note
assert "differs from current HEAD" not in abbrev_note, abbrev_note

alias_repo = temporary / "alias-reservations"
alias_repo.mkdir()
with patch.object(dispatch.room, "records", return_value=[]):
    with ThreadPoolExecutor(max_workers=4) as pool:
        aliases = list(pool.map(lambda _: dispatch._reserve_alias(alias_repo, "alias-room", "worker", None, []),
                                range(4)))
assert len(set(aliases)) == 4, aliases
with patch.object(dispatch.room, "records", return_value=[
        {"room_event": {"kind": "join", "alias": aliases[0]}}]):
    try:
        dispatch._reserve_alias(alias_repo, "alias-room", "worker", aliases[0], [])
        raise AssertionError("duplicate explicit alias was accepted")
    except ValueError as error:
        assert "already used" in str(error), error
    next_alias = dispatch._reserve_alias(alias_repo, "alias-room", "worker", None,
                                         [{"refs": {"panel_alias": aliases[1]}}])
assert next_alias == "W5", next_alias
alias_registry = json.loads((alias_repo / ".oms/artifacts/panel-aliases.json").read_text())
reserved_aliases = [row["alias"] for row in alias_registry["reservations"]]
assert aliases[1] not in reserved_aliases and aliases[0] in reserved_aliases, alias_registry
assert dispatch.ROLE_ALIAS_LETTERS["worker"] == "W"

warning_plan = temporary / "dispatch-warning-plan"
(warning_plan / ".oms/plan").mkdir(parents=True)
(warning_plan / ".oms/plan/tasks.json").write_text(
    '{"tasks":{"verified":{"verify":"test -f value.txt","allowed_paths":["scripts/a.sh"],'
    '"forbidden_paths":["secrets/"]},"empty":{"verify":""}}}')
assert dispatch._plan_task_paths(warning_plan, "verified") == (["scripts/a.sh"], ["secrets/"])
assert dispatch._plan_task_paths(warning_plan, "missing") == ([], [])
assert dispatch._plan_task_has_verifier(warning_plan, "verified")
assert not dispatch._plan_task_has_verifier(warning_plan, "empty")
assert not dispatch._plan_task_has_verifier(warning_plan, "missing")
warning_route = {"kind": "worker", "provider": "codex", "model": "test-model",
                 "role": "builder", "workload": "light", "effort": "low",
                 "access": "write", "purpose": "implement"}
for task, scopes, expected in (("missing", None, 1), ("verified", None, 0), ("missing", ["value.txt"], 0)):
    warning = io.StringIO()
    with patch.object(panel, "selected_route", return_value=warning_route), \
         patch.object(panel, "safe_label", side_effect=lambda value: value), \
         patch.object(panel, "child_environment", return_value={}), \
         patch.object(dispatch.room, "resolve", return_value=None), \
         patch.object(panel, "routing_status", return_value={}), \
         patch.object(panel, "alternate_route", return_value=None), \
         patch.object(panel, "run_dispatch", return_value=0), redirect_stderr(warning):
        dispatch.dispatch(warning_plan, "codex", "builder", "light", "write", "implement",
                          brief="scope is not parsed from this prose", verify=None, task_id=task,
                          dry_run=True, scopes=scopes)
    assert warning.getvalue().count("WARN:") == expected, warning.getvalue()

# A continuation keeps the latest write call's alias when a reviewer has since
# written a newer call row for the same task.
continued_brief = temporary / "continued-writer-brief.md"
continued_brief.write_text("Continue the writer round.\n")
continuation_rows = [
    {"attempt_id": "writer-round-1", "task_id": "continued-task",
     "refs": {"panel_alias": "B211", "panel_room_id": "continued-room", "panel_access": "write"}},
    {"attempt_id": "review-round-1", "task_id": "continued-task",
     "refs": {"panel_alias": "R444", "panel_room_id": "continued-room", "panel_access": "read"}},
]
continued_alias = {}
with patch.object(panel, "resolve_call_alias", side_effect=lambda repo, room, task: task), \
        patch.object(panel, "owning_attempt", return_value=[]), \
        patch.object(panel, "selected_route", return_value=warning_route), \
        patch.object(panel, "safe_label", side_effect=lambda value: value), \
        patch.object(panel, "child_environment", return_value={"OMS_ROOM_ID": "continued-room"}), \
        patch.object(dispatch.room, "resolve", return_value="continued-room"), \
        patch.object(dispatch.room, "same_room", return_value=True), \
        patch.object(panel, "events", return_value=json.dumps(continuation_rows)), \
        patch.object(dispatch, "_require_assigned_route"), \
        patch.object(panel, "continuation", return_value=(continued_brief, None, "continued")), \
        patch.object(dispatch, "_task_findings_brief", side_effect=lambda repo, tasks, brief: brief), \
        patch.object(panel, "routing_status", return_value={}), \
        patch.object(panel, "alternate_route", return_value=None), \
        patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: continued_alias.update(alias=args[17])):
    dispatch.dispatch(warning_plan, "codex", "builder", "light", "write", "implement",
                      brief="unused.md", continue_task="continued-task")
assert continued_alias.get("alias") == "B211", continued_alias

# A judge-only history is not a writer alias inheritance. Explicit aliases on
# that continuation still go through the reservation/collision check.
judge_only_rows = [{"attempt_id": "judge-round-1", "task_id": "judge-task",
                    "refs": {"panel_alias": "R444", "panel_room_id": "continued-room", "panel_access": "read"}},
                   {"attempt_id": "other-writer", "task_id": "other-task",
                    "refs": {"panel_alias": "B211", "panel_room_id": "continued-room", "panel_access": "write"}}]
continuation_options = {}
continued_brief.write_text("Continue the judge-only task.\n")
with patch.object(panel, "resolve_call_alias", side_effect=lambda repo, room, task: task), \
        patch.object(panel, "owning_attempt", return_value=[]), \
        patch.object(panel, "selected_route", return_value=warning_route), \
        patch.object(panel, "safe_label", side_effect=lambda value: value), \
        patch.object(panel, "child_environment", return_value={"OMS_ROOM_ID": "continued-room"}), \
        patch.object(dispatch.room, "resolve", return_value="continued-room"), \
        patch.object(dispatch.room, "same_room", return_value=True), \
        patch.object(panel, "events", return_value=json.dumps(judge_only_rows)), \
        patch.object(dispatch, "_require_assigned_route"), \
        patch.object(panel, "continuation", return_value=(continued_brief, None, "continued")), \
        patch.object(dispatch, "_task_findings_brief", side_effect=lambda repo, tasks, brief: brief), \
        patch.object(panel, "routing_status", return_value={}), \
        patch.object(panel, "alternate_route", return_value=None), \
        patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: continuation_options.update(kwargs)):
    dispatch.dispatch(warning_plan, "codex", "builder", "light", "write", "implement",
                      brief="unused.md", continue_task="judge-task", alias="B211")
assert continuation_options.get("continue_call") is False, continuation_options
try:
    with patch.object(dispatch.room, "records", return_value=[]):
        dispatch._reserve_alias(alias_repo, "continued-room", "worker", "B211", judge_only_rows)
    raise AssertionError("explicit alias owned by another task was accepted")
except ValueError as error:
    assert "already used" in str(error), error
try:
    panel.run_dispatch(root, "codex", "worker", "read", "inspect", None,
                       "bash tests/scripts-smoke.sh --only test_oms_allowlist_stays_in_sync_with_scripts",
                       None, True, None, {"kind": "worker", "purpose": "investigate"}, None,
                       None, None, work_repo=execution_repo)
    raise AssertionError("verifier present only in source repo was accepted")
except ValueError as error:
    assert "missing suite file" in str(error), error
# A write dispatch warns about a `python -m` module the probe cannot find, and is never refused for it.
def dispatch_verify(command, scope=("tests/",)):
    """Dry-run a write dispatch; the stub route fails with KeyError only after every verify check passed."""
    captured = io.StringIO()
    try:
        with redirect_stderr(captured):
            panel.run_dispatch(root, "codex", "builder", "write", "x", None, command,
                               None, True, None, {"kind": "worker", "purpose": "implement"}, None, None,
                               list(scope))
        raise AssertionError("stub route was expected to stop the dispatch: " + repr(command))
    except KeyError:
        return captured.getvalue()
for command in ("python3 -m surely_missing_module_xyz", "python3 -m surely_missing_module_xyz.sub && true"):
    interpreter = command.split()[0]
    expected = ("warning: verify may fail: %s cannot find module surely_missing_module_xyz\n" % interpreter
                if shutil.which(interpreter) else "")  # an interpreter that cannot run is no claim
    assert dispatch_verify(command) == expected, command
# Absence checks and every other verify shape are not refused, and stay silent.
for quiet in ("test ! -e tests/surely-missing-preflight-negative.py", "python3 -m json.tool", "python3 -m", "",
              "PYTHONPATH=scripts/lib python3 -m surely_missing_module_xyz",
              "env python3 -m surely_missing_module_xyz",
              "export PYTHONPATH=scripts/lib; python3 -m surely_missing_module_xyz",
              "true || python3 -m surely_missing_module_xyz",
              "true && python3 -m surely_missing_module_xyz",
              "python3 -m surely_missing_module_xyz\ntrue",
              "python3 -m surely_missing_module_xyz || true",
              "venv/bin/python3 -m surely_missing_module_xyz",
              'printf "%s\\n" python3 -m surely_missing_module_xyz'):
    assert dispatch_verify(quiet) == "", quiet
# The probe finds a module through the inherited PYTHONPATH and the repo root, and runs no package code.
sentinel_root = Path(temporary) / "sentinel-pkg"
(sentinel_root / "sentinelpkg").mkdir(parents=True)
(sentinel_root / "sentinelpkg" / "__init__.py").write_text(
    "from pathlib import Path\nPath(__file__).with_name('ran').write_text('x')\n")
with patch.dict(os.environ, {"PYTHONPATH": str(sentinel_root)}):
    assert dispatch_verify("python3 -m sentinelpkg.sub") == ""
    os.environ.pop("PYTHONPATH")
    assert dispatch_verify("python3 -m panel_dispatch") != ""
with patch.dict(os.environ, {"PYTHONPATH": str(Path(root) / "scripts/lib")}):
    assert dispatch_verify("python3 -m panel_dispatch") == ""
assert dispatch_verify("python3 -m scripts.lib.surely_missing_module_xyz") == ""
assert not (sentinel_root / "sentinelpkg" / "ran").exists(), "probe executed package initialization"
# BEGIN verify selector validation
for option, typo, expected in (
        ("--section", "room-protocol", "core/room-protocol"),
        ("--case", "reviewed-result acceptance compositio", "reviewed-result acceptance composition")):
    command = "bash tests/operator-tools-smoke.sh "
    if option == "--case":
        command += "--section dispatch-results "
    try:
        panel._validate_dispatch_verifier(root, command + option + " " + shlex.quote(typo))
        raise AssertionError("unknown selector was accepted: " + typo)
    except ValueError as error:
        assert "did you mean: " + expected in str(error), error
for case in ("no such case", "", "0", "reviewed-result acceptance composition (invalid suffix)."):
    try:
        panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section dispatch-results --case "
                                          + shlex.quote(case))
        raise AssertionError("unknown operator case was accepted: " + repr(case))
    except ValueError as error:
        assert str(error).startswith("unknown --case value(s): "), error
        assert "reviewed-result acceptance composition" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section dispatch-results"
                                  " --case 'reviewed-result acceptance composition'")
panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh"
                                  " --case 'reviewed-result acceptance composition' --section dispatch-results")
try:
    panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section not-a-section")
    raise AssertionError("unknown operator section was accepted")
except ValueError as error:
    assert "valid names" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section dispatch-exit,dispatch-results,core/dashboard")
try:
    panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section core/room-protocol --section salvage")
    raise AssertionError("repeated operator section was accepted")
except ValueError as error:
    assert "one comma-separated list" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section core/room-protocol,salvage")
try:
    panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only test_missing_smoke_case")
    raise AssertionError("unknown smoke test was accepted")
except ValueError as error:
    assert "valid names" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only test_oms_allowlist_stays_in_sync_with_scripts")
# Selectors after a shell separator belong to the next suite, not the first one.
panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only test_oms_allowlist_stays_in_sync_with_scripts"
                                  " && bash tests/operator-tools-smoke.sh --section dispatch-exit")
try:
    panel._validate_dispatch_verifier(root, "bash tests/run-smoke-shard.sh --only test_missing_smoke_case")
    raise AssertionError("unknown direct smoke-runner test was accepted")
except ValueError as error:
    assert "valid names" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/run-smoke-shard.sh --only test_oms_allowlist_stays_in_sync_with_scripts")
panel._validate_dispatch_verifier(root, "bash tests/autonomy-plan-run-smoke.sh --only test_context_pack")
# The runner word-splits one --only argument, so a space-separated pair passes; a comma is not a separator.
panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only 'test_poll_interval_adapts_to_elapsed_and_remaining test_file_lock_missing_poll_helper_falls_back'")
try:
    panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only 'test_poll_interval_adapts_to_elapsed_and_remaining test_missing_smoke_case'")
    raise AssertionError("pair with an unknown smoke test was accepted")
except ValueError as error:
    assert str(error).startswith("unknown --only value(s): test_missing_smoke_case; "), error
try:
    panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only 'test_poll_interval_adapts_to_elapsed_and_remaining,test_file_lock_missing_poll_helper_falls_back'")
    raise AssertionError("comma-separated smoke tests were accepted")
except ValueError as error:
    assert str(error).startswith("unknown --only value(s): test_poll_interval_adapts_to_elapsed_and_remaining,test_file_lock_missing_poll_helper_falls_back; "), error
try:
    panel._validate_dispatch_verifier(root, "bash tests/scripts-smoke.sh --only 'test_poll_interval_adapts_to_elapsed_and_remaining\ttest_file_lock_missing_poll_helper_falls_back'")
    raise AssertionError("tab-separated smoke tests were accepted")
except ValueError as error:
    assert str(error).startswith("unknown --only value(s): "), error


def exercise_continuation_verifier(temporary):
    execution = temporary / "carried-verifier-execution"
    artifacts = temporary / "carried-verifier-artifacts"
    (execution / "tests").mkdir(parents=True)
    (artifacts / ".oms/artifacts").mkdir(parents=True)
    for suite in ("scripts-smoke.sh", "autonomy-plan-run-smoke.sh", "run-smoke-shard.sh"):
        (execution / "tests" / suite).write_text("# run-smoke-shard.sh\ntest_base() {\n}\n")
    body = (
        "diff --git a/tests/scripts-smoke.sh b/tests/scripts-smoke.sh\n"
        "--- a/tests/scripts-smoke.sh\n+++ b/tests/scripts-smoke.sh\n"
        "@@ -1,2 +1,3 @@\n test_context_only() {\n-test_removed_only() {\n"
        "+test_carried_one() {\n+test_carried_two() {\n"
        "diff --git a/tests/autonomy-plan-run-smoke.sh b/tests/autonomy-plan-run-smoke.sh\n"
        '--- "a/tests/autonomy-plan-run-smoke.sh"\n+++ "b/tests/autonomy-plan-run-smoke.sh"\n'
        "@@ -0,0 +1 @@\n+test_other_suite() {\n"
        "diff --git a/docs/example.sh b/docs/example.sh\n"
        "--- a/docs/example.sh\n+++ b/docs/example.sh\n"
        "@@ -0,0 +1 @@\n+test_unrelated() {\n"
    ).encode()
    patch_file = artifacts / ".oms/artifacts/carried.patch"
    patch_file.write_bytes(body)
    reference = {"patch": ".oms/artifacts/carried.patch", "sha256": hashlib.sha256(body).hexdigest(),
                 "base": "a" * 40, "attempt": "previous-worker"}
    before = {path: path.read_bytes() for parent in (execution, artifacts)
              for path in parent.rglob("*") if path.is_file()}
    # The evidence lives in the room repo; suite names come from the execution repo.
    with patch.dict(os.environ, {}, clear=True), \
            patch.object(panel, "work_repository", return_value=execution), \
            patch.object(panel, "routed_command", return_value=["fixture", "--repo", str(artifacts)]), \
            patch.object(panel, "_validate_dry_run_inputs"), redirect_stdout(io.StringIO()):
        assert panel.run_dispatch(artifacts, "codex", "builder", "write", "x", None,
                                  "bash tests/run-smoke-shard.sh --only test_carried_one", None, True, None,
                                  {"kind": "worker", "purpose": "implement"}, None, None, None,
                                  work_repo=execution, continuation_patch=reference) == 0
    for suite, selected in (("scripts-smoke.sh", "test_carried_one"),
                            ("run-smoke-shard.sh", "test_carried_two"),
                            ("autonomy-plan-run-smoke.sh", "test_other_suite"),
                            ("scripts-smoke.sh", "test_base test_carried_one test_carried_two")):
        panel._validate_dispatch_verifier(execution, "bash tests/%s --only '%s'" % (suite, selected),
                                          continuation_patch=reference, patch_repo=artifacts)
    for selected in ("test_context_only", "test_removed_only", "test_unrelated", "test_other_suite",
                     "test_outside_hunk", "test_carried_one,test_carried_two",
                     "test_carried_one\ttest_carried_two", "", "0"):
        try:
            panel._validate_dispatch_verifier(execution, "bash tests/scripts-smoke.sh --only '%s'" % selected,
                                              continuation_patch=reference, patch_repo=artifacts)
            raise AssertionError("invalid carried selector was accepted: " + repr(selected))
        except ValueError as error:
            assert str(error).startswith("unknown --only value(s): "), error
    for candidate in (None, {}, dict(reference, sha256="0" * 64)):
        try:
            panel._validate_dispatch_verifier(execution, "bash tests/scripts-smoke.sh --only test_carried_one",
                                              continuation_patch=candidate, patch_repo=artifacts)
            raise AssertionError("unverified carried test was accepted")
        except ValueError:
            pass
    for candidate_body in (b"", body.replace(b"@@ -1,2 +1,3 @@", b"@@ invalid @@"),
                           b"+++ b/tests/scripts-smoke.sh\n@@ -0,0 +0,0 @@\n+test_carried_one() {\n",
                           b"+++ b/tests/scripts-smoke.sh\n@@ -0,0 +1 @@\n+# end\n+test_carried_one() {\n",
                           b"+++ b/docs/example.sh\n@@ -0,0 +1,2 @@\n+++ b/tests/scripts-smoke.sh\n+test_carried_one() {\n"):
        patch_file.write_bytes(candidate_body)
        candidate = dict(reference, sha256=hashlib.sha256(candidate_body).hexdigest())
        try:
            panel._validate_dispatch_verifier(execution, "bash tests/scripts-smoke.sh --only test_carried_one",
                                              continuation_patch=candidate, patch_repo=artifacts)
            raise AssertionError("empty or malformed patch authorized a carried test")
        except ValueError:
            pass
    patch_file.write_bytes(body)
    assert before == {path: path.read_bytes() for parent in (execution, artifacts)
                      for path in parent.rglob("*") if path.is_file()}, "validation changed repository files"


exercise_continuation_verifier(temporary)
# END verify selector validation

for owner in panel.PROVIDERS:
    env = dict(environment, OMS_AGENT=owner)
    peer = "claude" if owner == "codex" else "codex"
    command = panel.peer_command("ask", owner, project, prompt="Inspect value.txt without edits.")
    answer = call(command, env)
    assert "inspected the bounded repository task" in answer
    assert (project / "value.txt").read_text() == "old\n"
    write_env = dict(env, PANEL_TEST_MODE="write")
    command = panel.peer_command("delegate", owner, project, brief=brief,
                                 verify="test \"$(tail -1 value.txt)\" = new")
    assert "--graph-context" in command, command
    answer = call(command, write_env)
    assert re.search(r"^verify: exit 0 \([0-9]+s\)$", answer, re.M), answer
    patch_match = re.search(r"^patch: (.+)$", answer, re.M)
    assert patch_match and "+new" in Path(patch_match.group(1)).read_text(), answer
    assert (project / "value.txt").read_text() == "old\n", "worker edited primary"
    answer = call(panel.peer_command("review", owner, project, prompt="Review value.txt.",
                                     verify="test -f value.txt"), env)
    assert "pass" in answer
    turns = json.loads(call(["bash", str(panel.ENTRY), "thread", "--repo", str(project), "show", "--json"], env))["turns"]
    assert any(t.get("provider") == peer and t["role"] == "answer" for t in turns)

rows = json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo", str(project),
                        "--json", "list", "30"]))["rows"]
assert {r["provider"] for r in rows if r["kind"] == "delegate"} == set(panel.PROVIDERS)
assert {r["provider"] for r in rows if r["kind"] == "review"} == set(panel.PROVIDERS)

# Panel preparation builds a missing canonical graph when possible and fails
# open when the bounded best-effort subprocess cannot finish.
graph_fixture, _ = continuation_git_repo("panel-graph-missing")
(graph_fixture / "source.py").write_text("def fixture(): return 1\n")
shadow_marker = temporary / "pathlib-shadow-imported"
(graph_fixture / "pathlib.py").write_text("open(%r, 'w').write('imported')\n" % str(shadow_marker))
subprocess.run(["git", "-C", str(graph_fixture), "add", "source.py", "pathlib.py"], check=True)
subprocess.run(["git", "-C", str(graph_fixture), "commit", "-qm", "graph fixture source"], check=True)
outside_source = temporary / "graph-outside" / "module.py"
outside_source.parent.mkdir()
outside_source.write_text("def outside_secret(): return 'outside'\n")
(graph_fixture / "src").symlink_to(outside_source.parent, target_is_directory=True)
outside_blob = subprocess.check_output(["git", "-C", str(graph_fixture), "hash-object", "-w", "--stdin"],
                                       input=outside_source.read_bytes(), text=False).decode().strip()
subprocess.run(["git", "-C", str(graph_fixture), "update-index", "--add", "--cacheinfo",
                "100644," + outside_blob + ",src/module.py"], check=True)
subprocess.run(["git", "-C", str(graph_fixture), "commit", "-qm", "tracked source under symlink"], check=True)
assert "src/module.py" in subprocess.check_output(
    ["git", "-C", str(graph_fixture), "ls-files", "--cached"], text=True).splitlines()
with patch.dict(os.environ, {"OMS_LOCK_DIR": str(temporary / "graph-locks"), "OMS_HARNESS_CHILD": "0",
                            "OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}):
    dispatch._prepare_panel_graph(graph_fixture)
assert (graph_fixture / ".oms/project-graph/graph.json").is_file()
graph_manifest = json.loads((graph_fixture / ".oms/project-graph/manifest.json").read_text())
assert "src/module.py" not in graph_manifest["files"], "graph read a tracked file through a symlink ancestor"
assert not shadow_marker.exists(), "graph warm-up imported repository pathlib.py"
timeout_fixture = temporary / "panel-graph-timeout"
timeout_fixture.mkdir()
with patch.object(dispatch.subprocess, "run", side_effect=subprocess.TimeoutExpired("graph", 20)):
    dispatch._prepare_panel_graph(timeout_fixture)
assert not (timeout_fixture / ".oms/project-graph").exists()
# wait=False returns after starting exactly one detached child; the autobuild gate spawns none.
with patch.object(dispatch.subprocess, "Popen") as detached, patch.object(dispatch.subprocess, "run") as waited:
    dispatch._prepare_panel_graph(timeout_fixture, wait=False)
    assert detached.call_count == 1 and not waited.called, (detached.call_args_list, waited.call_args_list)
    detached_kwargs = detached.call_args.kwargs
    assert detached_kwargs["stdin"] == subprocess.DEVNULL and detached_kwargs["stdout"] == subprocess.DEVNULL
    assert detached_kwargs.get("start_new_session") is True if os.name == "posix" else "creationflags" in detached_kwargs
    with patch.dict(os.environ, {"OMS_GRAPH_AUTOBUILD": "0"}):
        dispatch._prepare_panel_graph(timeout_fixture, wait=False)
        dispatch._prepare_panel_graph(timeout_fixture)
    assert detached.call_count == 1 and not waited.called
with patch.object(dispatch.subprocess, "Popen", side_effect=OSError("no spawn")):
    dispatch._prepare_panel_graph(timeout_fixture, wait=False)

# Dry runs return before warming any graph cache.
preview_route = {"kind": "worker", "provider": "codex", "model": "gpt-6-luna", "effort": "low",
                 "role": "worker", "owner": "codex", "workload": "routine", "access": "write",
                 "purpose": "implement"}
preview_brief = temporary / "graph-preview-brief.md"
preview_brief.write_text("Preview only.\n")
preview_record = {"attempt_id": "preview-main", "tool": "panel-main", "provider": "codex",
                  "terminal": False, "refs": {"panel_room_participant": "preview-caller",
                                               "panel_room_id": "preview-room"}}
with patch.dict(os.environ, {"OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}), \
        patch.object(panel, "_validate_dispatch_verifier"), \
        patch.object(panel, "_validate_dry_run_inputs"), \
        patch.object(panel, "child_environment", return_value={"OMS_ROOM_ID": "preview-room",
                                                               "OMS_ROOM_PARTICIPANT": "preview-caller",
                                                               "OMS_PANEL_MAIN_ATTEMPT": "preview-main"}), \
        patch.object(panel, "events", return_value=json.dumps(preview_record)), \
        patch.object(panel, "active_mains", return_value={"preview-caller": preview_record}), \
        patch.object(dispatch.room, "resolve", return_value="preview-room"), \
        patch.object(dispatch.room, "records", return_value=[]), \
        patch.object(dispatch.room, "project", return_value={}), \
        patch.object(dispatch.room, "participant", return_value={"role": "main", "provider": "codex"}), \
        patch.object(dispatch.room, "same_room", return_value=True), \
        patch.object(dispatch.room, "declared_scopes"), \
        patch.object(panel, "scope_overlaps", return_value=[]), \
        patch.object(dispatch, "_prepare_panel_graph", side_effect=AssertionError("dry run warmed graph")), \
        redirect_stderr(io.StringIO()), redirect_stdout(io.StringIO()):
    assert dispatch.run_dispatch(root, "codex", "worker", "write", None, str(preview_brief),
                                 "true", "graph-preview", True, None, preview_route, "preview-caller", None,
                                 ["source.py"],
                                 work_repo=root) == 0

# A stale graph tracked in the canonical checkout blocks refresh, preserves all
# bytes, and does not prevent the real dispatch launch path from proceeding.
tracked_graph_repo, _ = continuation_git_repo("panel-tracked-graph-dispatch")
tracked_graph = tracked_graph_repo / ".oms/project-graph"
tracked_graph.mkdir(parents=True)
(tracked_graph / "graph.json").write_bytes(b'{"tracked":"graph"}\n')
(tracked_graph / "manifest.json").write_bytes(b'{"tracked":"manifest"}\n')
(tracked_graph / "cache.json").write_bytes(b'{"tracked":"cache"}\n')
subprocess.run(["git", "-C", str(tracked_graph_repo), "add", ".oms/project-graph"], check=True)
subprocess.run(["git", "-C", str(tracked_graph_repo), "commit", "-qm", "tracked graph fixture"], check=True)
tracked_before = {path.name: path.read_bytes() for path in tracked_graph.iterdir()}
tracked_route = dict(preview_route, access="read", purpose="explain")
graph_state_home = temporary / "graph-state-home"
graph_state_home.mkdir(mode=0o700)

class FinishedDispatchProcess:
    pid = os.getpid()
    def __init__(self):
        self.stdout = io.BytesIO()
        self.stderr = io.BytesIO()
    def poll(self): return 0
    def wait(self, timeout=None): return 0

with patch.dict(os.environ, {"OMS_LOCK_DIR": str(temporary / "graph-locks"), "XDG_STATE_HOME": str(graph_state_home),
                            "OMS_HARNESS_CHILD": "0",
                            "OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}):
    tracked_log_fd = os.open(os.devnull, os.O_WRONLY)
    import types
    launch_subprocess = types.SimpleNamespace(**vars(subprocess))
    launch_subprocess.Popen = lambda *args, **kwargs: FinishedDispatchProcess()
    with patch.object(panel, "work_repository", side_effect=lambda path: Path(path)), \
            patch.object(panel, "child_environment", return_value={}), \
            patch.object(panel, "plan_link", return_value=None), \
            patch.object(panel, "_prepare_dispatch_call", return_value=None), \
            patch.object(panel, "_dispatch_output_log", return_value=("", tracked_log_fd)), \
            patch.object(panel, "_save_dispatch_output"), \
            patch.object(panel, "_stop_dispatch", return_value=True), \
            patch.object(panel, "_observe_dispatch_session"), \
            patch.object(panel, "_observe_dispatch_descendants"), \
            patch.object(panel, "_print_dispatch_next_ready"), \
            patch.object(dispatch, "subprocess", launch_subprocess), \
            patch.object(panel, "_validate_dispatch_verifier"):
        assert dispatch.run_dispatch(tracked_graph_repo, "codex", "worker", "read", "Inspect source.", None,
                                     None, "tracked-graph-dispatch", False, None, tracked_route,
                                     None, None, [], work_repo=tracked_graph_repo) == 0
    os.close(tracked_log_fd)
assert {path.name: path.read_bytes() for path in tracked_graph.iterdir()} == tracked_before

# A real dispatch reaches the graph warm-up with an untrusted pathlib.py in
# the repository; neither controller nor isolated preparation imports it.
shadow_route = dict(tracked_route)
fsmonitor_marker = temporary / "fsmonitor-command-executed"
subprocess.run(["git", "-C", str(graph_fixture), "config", "core.fsmonitor",
                "touch " + str(fsmonitor_marker)], check=True)
with patch.dict(os.environ, {"OMS_LOCK_DIR": str(temporary / "graph-locks"), "XDG_STATE_HOME": str(graph_state_home),
                            "OMS_HARNESS_CHILD": "0",
                            "OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}):
    shadow_log_fd = os.open(os.devnull, os.O_WRONLY)
    import types
    launch_subprocess = types.SimpleNamespace(**vars(subprocess))
    launch_subprocess.Popen = lambda *args, **kwargs: FinishedDispatchProcess()
    with patch.object(panel, "work_repository", side_effect=lambda path: Path(path)), \
            patch.object(panel, "child_environment", return_value={}), \
            patch.object(panel, "plan_link", return_value=None), \
            patch.object(panel, "_prepare_dispatch_call", return_value=None), \
            patch.object(panel, "_dispatch_output_log", return_value=("", shadow_log_fd)), \
            patch.object(panel, "_save_dispatch_output"), \
            patch.object(panel, "_stop_dispatch", return_value=True), \
            patch.object(panel, "_observe_dispatch_session"), \
            patch.object(panel, "_observe_dispatch_descendants"), \
            patch.object(panel, "_print_dispatch_next_ready"), \
            patch.object(dispatch, "subprocess", launch_subprocess), \
            patch.object(panel, "_validate_dispatch_verifier"):
        assert dispatch.run_dispatch(graph_fixture, "codex", "worker", "read", "Inspect source.", None,
                                     None, "pathlib-shadow-dispatch", False, None, shadow_route,
                                     None, None, [], work_repo=graph_fixture) == 0
    os.close(shadow_log_fd)
assert not shadow_marker.exists(), "panel dispatch imported repository pathlib.py"
assert not fsmonitor_marker.exists(), "panel dispatch executed repository core.fsmonitor"

# Capture the actual child environment without invoking a provider.
for configured_timeout, expected_timeout in ((None, "60m"), ("17m", "17m"), ("", "60m"), ("0", "0"), ("60m", "60m"), ("malformed", "malformed")):
    scholar_route = allocate("codex", role="scholar", installed={"claude", "codex"})
    captured = []
    launch_subprocess = types.SimpleNamespace(**vars(subprocess))
    def capture_launch(*args, **kwargs):
        captured.append(dict(kwargs["env"]))
        return FinishedDispatchProcess()
    launch_subprocess.Popen = capture_launch
    initial = {} if configured_timeout is None else {"OMS_PEER_TIMEOUT": configured_timeout}
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "0", "OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}), \
            patch.object(panel, "work_repository", side_effect=lambda path: Path(path)), \
            patch.object(panel, "child_environment", return_value=initial), \
            patch.object(panel, "plan_link", return_value=None), \
            patch.object(panel, "_prepare_dispatch_call", return_value=None), \
            patch.object(panel, "_dispatch_output_log", return_value=("", os.devnull)), \
            patch.object(panel, "_save_dispatch_output"), \
            patch.object(panel, "_stop_dispatch", return_value=True), \
            patch.object(panel, "_observe_dispatch_session"), \
            patch.object(panel, "_observe_dispatch_descendants"), \
            patch.object(panel, "_print_dispatch_next_ready"), \
            patch.object(panel, "_validate_dispatch_verifier"), \
            patch.object(dispatch, "_prepare_panel_graph"), \
            patch.object(dispatch, "subprocess", launch_subprocess):
        with open(os.devnull, "wb") as sink, patch.object(panel, "_dispatch_output_log", return_value=("", sink.fileno())):
            assert dispatch.run_dispatch(graph_fixture, "codex", "scholar", "read", "Research.", None,
                                         None, "scholar-env", False, None, scholar_route,
                                         None, None, [], work_repo=graph_fixture) == 0
    observed, = captured
    assert observed["OMS_PANEL_ROLE"] == "researcher", observed
    assert observed["OMS_PANEL_STRATEGY"] == "scholar", observed
    assert observed["OMS_PANEL_PURPOSE"] == "research", observed
    assert observed["OMS_PEER_TIMEOUT"] == expected_timeout, observed

# Same-repository dispatch retains its per-role execution locations, with one
# recorded main per caller and read/write authority tested independently.
main_attempts = {}
for owner in panel.PROVIDERS:
    main_attempts[owner] = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project),
                               "start", "--provider", owner, "--tool", "panel-main", "--ref", "panel_role=main",
                               "--then", "starting", "--then", "working"]).strip()
cases = [("codex", "worker", "light", "read", "explain", "claude-haiku-5-5"),
         ("codex", "worker", None, "read", "explain", "claude-sonnet-5-5"),
         ("codex", "worker", "deep", "read", "investigate", "claude-opus-5-5"),
         ("claude", "builder", None, "write", None, "gpt-6.1-terra"),
         ("claude", "advisor", None, "read", None, "gpt-6.1-sol"),
         ("codex", "advisor", "deep", "read", None, "claude-opus-5-5"),
         ("claude", "reviewer", None, "read", None, "gpt-6.1-sol"),
         ("claude", "security-auditor", None, "read", None, "gpt-6-astra"),
         ("codex", "researcher", None, "read", None, "claude-haiku-5-5"),
         ("claude", "researcher", None, "read", None, "gpt-6-luna"),
         ("codex", "scholar", None, "read", None, "claude-opus-5-5"),
         ("claude", "scholar", None, "read", None, "gpt-6.1-sol"),
         ("claude", "scholar", "frontier", "read", None, "gpt-6-astra")]
for index, (owner, role, workload, access, purpose, expected) in enumerate(cases):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", role,
               "--owner", owner, "--access", access, "--task-id", "panel-route-%s" % index,
               "--label", "Inspect fixture source %s" % index, "--alias", "T%d" % index]
    request += (["--workload", workload] if workload else []) + (["--purpose", purpose] if purpose else [])
    if access == "write":
        request += ["--brief-file", str(brief), "--verify", "test \"$(tail -1 value.txt)\" = new"]
    else:
        request += ["--prompt", "Explain value.txt; judge only the supplied source evidence."]
        if role == "reviewer":
            request += ["--verify", "test -f value.txt"]
    before = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
    route = json.loads(call(request + ["--dry-run"]))
    assert route["route"]["model"] == expected and route["executes"] is False, route
    if role not in {"advisor", "reviewer"}:
        assert route["argv"][route["argv"].index("--role") + 1] == (
            role if role != "worker" else "analysis-worker"), route["argv"]
    env = dict(environment, OMS_AGENT=owner, OMS_PANEL_MAIN_ATTEMPT=main_attempts[owner])
    if access == "write":
        env["PANEL_TEST_MODE"] = "write"
        call(["git", "config", "diff.mnemonicPrefix", "true"])
    answer = call(request, env)
    if access == "write":
        assert "patch:" in answer, answer
        write_patch = re.search(r"^patch: (.+)$", answer, re.M).group(1)
        from panel_results import _patch_paths, _diffstat
        captured = Path(write_patch).read_bytes()
        assert _patch_paths(captured) == ["value.txt"]
        stat = _diffstat(project, {"patch": Path(write_patch).relative_to(project).as_posix(),
                                  "patch_sha256": hashlib.sha256(captured).hexdigest()})
        assert stat["files"] == [["value.txt", 1, 0]], stat
        call(["git", "config", "--unset", "diff.mnemonicPrefix"])
    assert (project / "value.txt").read_text() == "old\n", "role task edited primary"
    assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == before
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(row for row in attempts if row.get("task_id") == "panel-route-%s" % index)
    refs = attempt["refs"]
    assert refs["panel_role"] == panel.KINDS[role] and refs["panel_model"] == expected, attempt
    assert refs["panel_strategy"] == role, refs
    assert refs["panel_purpose"] == route["route"]["purpose"] and (role == "worker" or refs["panel_purpose"] in (
        role, "advise", "review", "research")), refs
    assert refs["panel_effort"] == route["route"]["effort"], refs
    assert refs["panel_access"] == access and refs["panel_label"] == "Inspect fixture source %s" % index, refs
    assert refs["panel_alias"] == "T%d" % index, refs
    retained = panel_results.results(project, "panel-route-%s" % index)["rows"][0]
    assert any(call.get("attempt_id") == attempt["attempt_id"] and call.get("alias") == "T%d" % index
               for call in retained["calls"]), retained
    assert attempt["parent_attempt_id"] == main_attempts[owner], attempt
    assert attempt["state"] == ("review" if access == "write" else "done"), attempt
    assert refs["panel_location"].startswith("worktree:") if role not in {"advisor", "reviewer"} else refs["panel_location"] == "repository", refs
    assert str(home) not in json.dumps(refs) and str(project) not in json.dumps(refs), refs
    if role in {"advisor", "scholar"}:
        retained = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                                    "--task-id", "panel-route-%s" % index, "--json"]))["rows"][0]
        artifact_kind = "call" if role == "advisor" else "delegate"
        answer_text = "inspected source" if role == "advisor" else "inspected the bounded repository task."
        assert any(c.get("kind") == artifact_kind and answer_text in c.get("answer", "")
                   for c in retained["calls"]), retained
        assert retained["acceptance"] is False
    observed = json.loads(worker_log.read_text().splitlines()[-1])
    assert expected in observed["argv"], observed
    if route["route"]["provider"] == "claude":
        assert observed["argv"][observed["argv"].index("--effort") + 1] == route["route"]["effort"], observed
    else:
        assert any("model_reasoning_effort" in arg and route["route"]["effort"] in arg
                   for arg in observed["argv"]), observed
    assert observed["parent"] == main_attempts[owner], observed
    if role not in {"advisor", "reviewer"}:
        assert observed["cwd"] != str(project), observed
    else:
        assert observed["cwd"] == str(project), observed

# Scratch-capable investigations return findings without entering patch review.
try:
    allocate("codex", access="write", purpose="explain", installed=both)
    raise AssertionError("explain accepted write access")
except ValueError:
    pass
for owner in panel.PROVIDERS:
    scratch_task = "scratch-investigation-" + owner
    scratch_room = room.create(project, scratch_task, "Report-only investigation")
    room.join(project, scratch_room, main_attempts[owner], owner, "main")
    scratch_prompt = temporary / (scratch_task + ".prompt")
    scratch_brief = temporary / (scratch_task + ".md")
    scratch_brief.write_text("Inspect value.txt; return findings without repository edits.\n")
    scratch_env = dict(environment, OMS_AGENT=owner, OMS_ROOM_ID=scratch_room,
                       OMS_ROOM_PARTICIPANT=main_attempts[owner],
                       OMS_PANEL_MAIN_ATTEMPT=main_attempts[owner], PANEL_TEST_PROMPT=str(scratch_prompt))
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", scratch_room,
               "--dispatch", "worker", "--owner", owner, "--purpose", "investigate", "--access", "write",
               "--brief-file", str(scratch_brief), "--verify", "test -f value.txt",
               "--task-id", scratch_task]
    route = json.loads(call(request + ["--dry-run"], scratch_env))
    assert route["argv"][route["argv"].index("--role") + 1] == "analysis-worker", route
    assert "--allow-no-change" in route["argv"] and "--read-only" not in route["argv"], route
    previous_calls = len(worker_log.read_text().splitlines())
    answer = call(request, scratch_env)
    assert "patch: none (no changes)" in answer, answer
    assert len(worker_log.read_text().splitlines()) == previous_calls + 1, "empty patch launched reviewer"
    assert "Return findings" in scratch_prompt.read_text(), scratch_prompt.read_text()
    observed = json.loads(worker_log.read_text().splitlines()[-1])
    if route["route"]["provider"] == "codex":
        assert "workspace-write" in observed["argv"], observed
    result = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                              "--task-id", scratch_task, "--json"], scratch_env))["rows"][0]
    assert result["process_state"] == "review" and not result["acceptance"], result
    assert result["representative_state"] == "findings awaiting owner judgment", result
    assert result["stages"]["verify"]["state"] == "passed", result
    report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                              "--room", scratch_room, "--json"], scratch_env))["dashboard"]
    events = [turn["room_event"] for turn in room.records(project, scratch_room) if turn.get("room_event")]
    event = next(event for event in events if event.get("kind") == "call-result")
    assert not event["needs_review"] and event["verify_status"] == "passed", event
    # A missing or digest-mismatched patch must never become empty success.
    import panel_review
    evidence_patch = project / event["patch"]
    for invalid_bytes in (b"tampered", None):
        if invalid_bytes is None:
            evidence_patch.unlink()
        else:
            evidence_patch.write_bytes(invalid_bytes)
        try:
            panel_review._call_result_event(project, scratch_room, event["call_id"], scratch_task, 0)
            raise AssertionError("invalid patch evidence became an empty result")
        except (OSError, ValueError):
            pass
        finally:
            evidence_patch.write_bytes(b"")
    from review_backlog import counts
    assert counts(report).get(main_attempts[owner]) == 1, report
    scratch_summary = temporary / (scratch_task + "-summary.md")
    scratch_summary.write_text("Findings: value.txt exists; no repository edits.\n")
    finalize = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--finalize",
                "--task-id", scratch_task, "--owner", owner, "--summary-file", str(scratch_summary)]
    refused = subprocess.run(finalize + ["--outcome", "accepted", "--verify", "true",
                                         "--evidence", event["artifact"]], env=scratch_env,
                             capture_output=True, text=True,
                             timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
    assert refused.returncode != 0, "findings satisfied accepted"
    finalized = json.loads(call(finalize + ["--outcome", "completed"], scratch_env))
    assert finalized["outcome"] == "completed", finalized
    assert counts(json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                   "--room", scratch_room, "--json"], scratch_env))["dashboard"]).get(main_attempts[owner], 0) == 0
    # A short continuation keeps purpose=investigate, so a changed repository file is still rejected.
    again_task = scratch_task + "-again"
    again_room = room.create(project, again_task, "Report-only continuation")
    room.join(project, again_room, main_attempts[owner], owner, "main")
    again_env = dict(scratch_env, OMS_ROOM_ID=again_room)
    first_round = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", again_room,
                   "--dispatch", "worker", "--owner", owner, "--purpose", "investigate", "--access", "write",
                   "--brief-file", str(scratch_brief), "--scope", "value.txt", "--verify", "test -f value.txt",
                   "--task-id", again_task]
    call(first_round, again_env)
    short = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", again_room, "--owner", owner,
             "--brief-file", str(scratch_brief), "--continue", again_task]
    plan = json.loads(call(short + ["--dry-run"], again_env))
    assert plan["route"]["purpose"] == "investigate" and "--allow-no-change" in plan["argv"], plan
    assert plan["argv"][plan["argv"].index("--role") + 1] == "analysis-worker", plan
    edited = subprocess.run(short, env=dict(again_env, PANEL_TEST_MODE="write"), capture_output=True, text=True,
                            timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
    assert edited.returncode != 0 and "report-only investigation changed repository files" in (
        edited.stdout + edited.stderr), edited

# A researcher keeps the cheap family, read-only strategy and source evidence.
from panel_routing import newest_route_model
assert newest_route_model("codex", "gpt-6-luna", ["gpt-6.2-luna", "gpt-6.10-luna", "gpt-7-astra"]) == "gpt-6.10-luna"
assert newest_route_model("codex", "gpt-6-astra", ["gpt-6-astra", "gpt-6.2-astra", "gpt-6.1-terra"]) == "gpt-6.2-astra"
assert newest_route_model("codex", "gpt-6.1-terra", ["gpt-5.6-terra", "gpt-6.1-terra", "gpt-6.1-sol"]) == "gpt-6.1-terra"
assert newest_route_model("claude", "claude-haiku-5-5", ["claude-haiku-5-6", "claude-opus-9-9"]) == "claude-haiku-5-6"
codex_only = {"provider": "codex", "binary": "codex", "installed": True, "exact_model_override": True, "access": ["read", "write"], "models": ["gpt-6-sol"],
              "catalog_models": ["gpt-6-sol", "gpt-6.1-sol", "gpt-6.2-sol", "gpt-6.3-astra"]}
with patch.object(panel, "provider_catalog", return_value=[codex_only]):
    assert panel.selected_route("codex", "worker", "deep", "read", "investigate")["model"] == "gpt-6.2-sol"
    assert panel.selected_route("codex", "worker", "light", "read", "explain")["model"] == "gpt-6-luna"
    assert panel.selected_route("codex", "reviewer", None, "read", None)["provider"] == "codex"
    explicit = panel.selected_route("claude", "worker", None, "read", None, "codex")
    assert explicit["provider"] == "codex" and explicit["model"] == "gpt-6.1-terra" and explicit["effort"] == "medium"
# Like the shell's routable set, a family whose newest member is a major generation behind the newest
# Codex family model in the catalog is not routed: the tier's other provider serves, or dispatch is refused.
from panel_routing import routable_route_model
seven = ["gpt-7-astra", "gpt-6.1-terra", "gpt-6.2-sol", "gpt-6-astra"]
assert routable_route_model("codex", "gpt-6-astra", seven) == "gpt-7-astra"
for configured in ("gpt-6.1-terra", "gpt-6.1-sol", "gpt-6-luna"):
    try:
        routable_route_model("codex", configured, seven)
        raise AssertionError("a family behind gpt-7 was routed: " + configured)
    except ValueError as error:
        assert "not routed" in str(error), error
assert routable_route_model("codex", "gpt-6.1-terra", []) == "gpt-6.1-terra"
assert routable_route_model("codex", "gpt-6.1-terra", ["gpt-6-sol", "gpt-7..1-astra", "gpt-7-nova"]) == "gpt-6.1-terra"
assert routable_route_model("codex", "gpt-7-terra", ["gpt-6.1-terra", "gpt-6-astra"]) == "gpt-7-terra"
assert routable_route_model("claude", "claude-haiku-5-5", ["claude-opus-9-9"]) == "claude-haiku-5-5"
seven_row = dict(codex_only, catalog_models=seven)
with patch.object(panel, "provider_catalog", return_value=[seven_row]):
    assert panel.selected_route("codex", "worker", "frontier", "read", "investigate")["model"] == "gpt-7-astra"
    try:
        panel.selected_route("claude", "worker", None, "read", None, "codex")
        raise AssertionError("routine routed Terra 6.1 beside gpt-7")
    except ValueError as error:
        assert "gpt-6.1-terra" in str(error) and "not routed" in str(error), error
from panel_routing import alternate_route
automatic = allocate("claude", role="builder", tier="routine", access="write",
                     installed={"codex", "claude"})
alternate = alternate_route(automatic, {"codex", "claude"})
assert alternate and alternate["provider"] != automatic["provider"]
assert alternate["workload"] == automatic["workload"] == "routine", alternate
assert alternate_route(automatic, {automatic["provider"]}) is None
# The fallback obeys the weekly-quota rule allocate() enforces: 90% used or more is excluded,
# unknown or stale readings stay eligible, and without an eligible alternate nothing falls back.
other = alternate["provider"]
assert alternate_route(automatic, both, provider_status={other: {"weekly_used": 89}})["provider"] == other
assert alternate_route(automatic, both, provider_status={other: {"weekly_used": None}})["provider"] == other
assert alternate_route(automatic, both, provider_status={}) ["provider"] == other
assert alternate_route(automatic, both, provider_status={other: {"weekly_used": 90}}) is None
assert alternate_route(automatic, {"codex", "claude"}, {other: {"weekly_used": 97.2}}) is None
with patch.object(panel, "selected_route", return_value=explicit), \
        patch.object(panel, "provider_catalog", side_effect=AssertionError("explicit --to queried fallback")), \
        patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: args[-2]):
    assert panel.dispatch(project, "claude", "worker", None, "read", None,
                          prompt="explicit route", target="codex", model="gpt-6-luna") is None
recorded = {"codex": {"weekly_used": 90}, "claude": {"weekly_used": None}}
claude_candidate = {"provider": "claude", "binary": "claude", "installed": True, "models": [], "exact_model_override": True, "access": ["read", "write"]}
def dispatched_fallback(readings):
    catalog = [dict(row, installed=True) for row in (codex_only, claude_candidate)]
    with patch.object(panel, "selected_route", return_value=dict(automatic, provider="claude", model="claude-sonnet-5-5",
                                                                  binary="claude", effort="medium")), \
            patch.object(panel, "provider_catalog", side_effect=lambda name="all": [row for row in catalog if row["provider"] == name]), \
            patch.object(panel, "routing_status", return_value=readings) as status, \
            patch.object(panel, "run_dispatch", side_effect=lambda *args, **kwargs: args[-2]):
        result = panel.dispatch(project, "claude", "worker", None, "write", "implement", brief=str(fallback_brief),
                                verify="true")
        status.assert_called_once()
        return result
fallback_brief = temporary / "fallback-brief.md"
fallback_brief.write_text("scoped brief\n")
swapped = dispatched_fallback({})
assert swapped and swapped["provider"] == "codex", swapped
assert dispatched_fallback({"codex": {"weekly_used": 95}}) is None
def fixture_selected_route(*args, **kwargs):
    target = args[5] if len(args) > 5 else kwargs.get("target")
    catalog = [row for row in (codex_only, claude_candidate) if target is None or row["provider"] == target]
    with patch.object(panel, "provider_catalog", return_value=catalog):
        return panel.selected_route(*args, **kwargs)
with patch.object(panel, "provider_catalog", return_value=[codex_only, claude_candidate]), \
        patch.object(panel, "snapshot", return_value=({"room": {"id": "quota-room"}}, 0)), \
        patch("panel_metrics.collect", return_value=recorded) as metrics:
    selected = panel.selected_route("codex", "worker", None, "read", None,
                                    repo=project, room_id="quota-room", main="quota-main")
    assert selected["provider"] == "claude" and selected["model"] == "claude-sonnet-5-5"
    metrics.assert_called_once_with({"room": {"id": "quota-room"}}, "quota-main", repo=project)
with patch.object(panel, "provider_catalog", return_value=[codex_only]), \
        patch("panel_metrics.collect", side_effect=AssertionError("explicit provider queried usage")):
    assert panel.selected_route("claude", "worker", None, "read", None, "codex", repo=project,
                                room_id="quota-room")["provider"] == "codex"
with patch.object(panel, "provider_catalog", return_value=[codex_only, claude_candidate]), \
        patch.object(panel, "snapshot", return_value=({}, 1)), \
        patch("panel_metrics.collect", side_effect=ValueError("unavailable reading")):
    assert panel.selected_route("codex", "worker", None, "read", None,
                                repo=project, room_id="quota-room")["provider"] == "claude"
with patch.object(panel, "provider_catalog", return_value=[seven_row, dict(claude_candidate, installed=True)]):
    assert panel.selected_route("claude", "worker", None, "read", None)["model"] == "claude-sonnet-5-5"
# Family exclusion is candidate filtering, not reselection: the provider it leaves still passes the
# quota guard, so a Claude at or past 90% weekly use is refused with the usual exhaustion error.
for used, expected in ((89, "claude-sonnet-5-5"), (90, None), (99, None)):
    with patch.object(panel, "provider_catalog", return_value=[seven_row, dict(claude_candidate, installed=True)]), \
            patch.object(panel, "snapshot", return_value=({"room": {"id": "quota-room"}}, 0)), \
            patch("panel_metrics.collect", return_value={"codex": {"weekly_used": 20}, "claude": {"weekly_used": used}}):
        try:
            selected = panel.selected_route("codex", "worker", None, "read", None,
                                            repo=project, room_id="quota-room", main="quota-main")
        except ValueError as error:
            assert expected is None and "all installed providers in the routine tier have <=10% weekly quota left" in str(error), (used, error)
        else:
            assert selected["model"] == expected, (used, selected)
with patch.object(panel, "provider_catalog", return_value=[dict(codex_only, installed=False)]):
    try:
        panel.selected_route("codex", "worker", None, "read", None)
        raise AssertionError("a route was chosen with no installed provider")
    except ValueError as error:
        assert "no installed provider serves the routine tier" in str(error), error
with patch.object(panel, "provider_catalog", return_value=[dict(codex_only, models=["gpt-6-luna"],
        catalog_models=["gpt-6-luna"] * 20 + ["gpt-6.2-luna"])]):
    assert panel.selected_route("codex", "researcher", None, "read", None)["model"] == "gpt-6.2-luna"
def fixture_selected_route(*args, **kwargs):
    target = kwargs.get("target", args[5] if len(args) > 5 else None)
    catalog = [row for row in (codex_only, claude_candidate) if target in (None, row["provider"])]
    with patch.object(panel, "provider_catalog", return_value=catalog):
        return panel.selected_route(*args, **kwargs)
try:
    panel.room.declared_scopes("researcher", ["src"])
    raise AssertionError("researcher write scope was accepted")
except ValueError:
    pass
def fixture_provider_catalog(providers=None):
    rows = [codex_only, claude_candidate]
    if providers:
        requested = set(providers.split(",")) if isinstance(providers, str) else set(providers)
        rows = [row for row in rows if row["provider"] in requested]
    return rows

with patch.object(panel, "provider_catalog", side_effect=fixture_provider_catalog):
    for target in ("codex", "claude"):
        preset = panel.selected_route("claude", "researcher", None, "read", None, target)
        command = panel.routed_command(panel.ENTRY, project, preset, prompt="Find cited evidence.")
        assert "--read-only" in command and command[command.index("--role") + 1] == "researcher"
        assert "--graph-context" in command, command
        assert preset["provider"] == target and preset["effort"] == "low" and preset["workload"] == "light"
        for options in ((None, "read", "implement", None, None, None),
                        (None, "write", None, None, None, None),
                        ("deep", "read", None, None, None, None),
                        (None, "read", None, None, None, "high"),
                        (None, "read", None, "codex", "gpt-6-astra", None),
                        (None, "read", None, "claude", "provider-default", None),
                        (None, "read", None, "grok", None, None),
                        ("light", "read", None, "codex", None, None)):
            workload, access, purpose, to, model, effort = options
            try:
                panel.selected_route("claude", "researcher", workload, access, purpose, to, model, effort)
                raise AssertionError("researcher authority/model override was accepted")
            except ValueError:
                pass
        for forbidden in ({"verify": "true"}, {"resume": "fixture-session"}):
            try:
                panel.routed_command(panel.ENTRY, project, preset, prompt="Lookup only.", **forbidden)
                raise AssertionError("researcher gate/continuation was accepted")
            except ValueError:
                pass
    # Only write-capable roles continue a session; judges keep a context apart from the implementer.
    for role in ("reviewer", "verifier", "security-auditor", "critic", "advisor", "release-engineer", "scholar"):
        judge = panel.selected_route("claude", role, None, "read", None)
        try:
            panel.routed_command(panel.ENTRY, project, judge, brief=str(brief), verify="true", resume="fixture-session")
            raise AssertionError("%s resumed a session" % role)
        except ValueError as error:
            assert "only write-capable roles continue" in str(error), error
    # Among read roles only a reviewer runs a verifier; the rest refuse an explicit one and name the reviewer.
    for role in ("verifier", "security-auditor", "critic", "worker", "advisor", "scholar"):
        try:
            panel.routed_command(panel.ENTRY, project, panel.selected_route("claude", role, None, "read", None),
                                 prompt="Check.", verify="exit 7")
            raise AssertionError("%s accepted an explicit verifier" % role)
        except ValueError as error:
            assert "reviewer" in str(error), (role, error)
    refactor = panel.selected_route("claude", "refactorer", None, "write", None)
assert "--resume-session" in panel.routed_command(panel.ENTRY, project, refactor, brief=str(brief), verify="true",
                                                  resume="fixture-session")

# Lifecycle admission rejects a forged researcher label on a write delegate before the native CLI runs.
for access, purpose, model, effort in (("write", "research", "gpt-6-luna", "low"),
                                     ("read", "implement", "gpt-6-luna", "low"),
                                     ("read", "research", "gpt-6-astra", "low"),
                                     ("read", "research", "gpt-6-luna", "high")):
    forged = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"],
                  OMS_PANEL_DISPATCH="1", OMS_PANEL_ROLE="researcher", OMS_PANEL_PURPOSE=purpose)
    request = ["bash", str(panel.ENTRY), "peer-delegate", "--repo", str(project), "--to", "codex",
               "--model", model, "--reasoning-effort", effort, "--brief-file", str(brief), "--verify", "test -f value.txt"]
    if access == "read":
        request.append("--read-only")
    before = worker_log.read_bytes()
    denied = subprocess.run(request, env=forged, capture_output=True, text=True)
    assert denied.returncode != 0 and worker_log.read_bytes() == before, denied.stdout + denied.stderr

# Omitted worker purpose follows access, and one effective ID binds the plan,
# argv, execution metadata and operator-visible launch line.
read_default = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                "--owner", "codex", "--prompt", "Explain the fixture source."]
write_default = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                 "--owner", "codex", "--access", "write", "--brief-file", str(brief),
                 "--verify", 'test "$(tail -1 value.txt)" = new', "--label", "Default implementation"]
worker_before = worker_log.read_bytes()
events_before = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"])
read_plan = json.loads(call(read_default + ["--dry-run"]))
write_plan = json.loads(call(write_default + ["--dry-run"]))
for planned, purpose in ((read_plan, "explain"), (write_plan, "implement")):
    ident = planned["task_id"]
    assert planned["route"]["purpose"] == purpose, planned
    assert re.fullmatch(r"panel-%s-[a-f0-9]{8}" % purpose, ident), ident
    assert planned["argv"][planned["argv"].index("--task-id") + 1] == ident, planned
    assert planned["executes"] is False
assert worker_log.read_bytes() == worker_before, "dry-run invoked a native fixture"
assert call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]) == events_before

write_output = call(write_default, dict(environment, OMS_AGENT="codex",
                                        OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"], PANEL_TEST_MODE="write"))
visible = re.findall(r"\btask_id=(panel-implement-[a-f0-9]{8})\b", write_output)
assert len(visible) == 1, write_output
generated_id = visible[0]
execution = json.loads(worker_log.read_text().splitlines()[-1])
assert execution["purpose"] == "implement" and execution["task_id"] == generated_id, execution
attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
attempt = next(row for row in attempts if row.get("task_id") == generated_id)
assert attempt["refs"]["panel_purpose"] == "implement", attempt

# Explicit purpose and task IDs remain exact.
explicit_id = "explicit-task-ID.7"
explicit = read_default + ["--purpose", "investigate", "--task-id", explicit_id]
explicit_plan = json.loads(call(explicit + ["--dry-run"]))
assert explicit_plan["task_id"] == explicit_id and explicit_plan["route"]["purpose"] == "investigate"
assert explicit_plan["argv"][explicit_plan["argv"].index("--task-id") + 1] == explicit_id

# The routine Pi route executes scoped edits through the declared transport.
pi_model = "openrouter/thinkingmachines/inkling:free"
catalog_file = capability_dir / "pi.models"
previous = catalog_file.read_bytes()
try:
    catalog_file.write_text(pi_model + "\n")
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
               "--owner", "codex", "--access", "write", "--purpose", "implement",
               "--brief-file", str(brief), "--verify", "test -f value.txt", "--task-id", "pi-write", "--label", "Implement with Pi"]
    private_plan = json.loads(call(request + ["--dry-run"]))
    assert private_plan["route"]["provider"] != "pi", private_plan
    pi_env = dict(environment, OMS_PANEL_PUBLIC_ROUTE_OPT_IN="1")
    native_plan = json.loads(call(request + ["--dry-run"], pi_env))
    assert native_plan["route"]["provider"] in {"claude", "codex"}, native_plan
    pi_request = request + ["--to", "pi", "--model", pi_model, "--reasoning-effort", "medium"]
    planned = json.loads(call(pi_request + ["--dry-run"], pi_env))
    assert planned["route"]["provider"] == "pi" and "--reasoning-effort" in planned["argv"]
    answer = call(pi_request, dict(pi_env, PANEL_TEST_MODE="write",
                                OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
    assert "patch:" in answer, answer
    observed = json.loads(worker_log.read_text().splitlines()[-1])
    assert pi_model in observed["argv"] and "--tools" in observed["argv"], observed
    assert observed["argv"][observed["argv"].index("--thinking") + 1] == "medium", observed
    assert (project / "value.txt").read_text() == "old\n"
finally:
    catalog_file.write_bytes(previous)

# Registered transports join the same task tree without a native launcher or
# changes to preset allocation. Lumen is a synthetic custom-adapter fixture.
external_base = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                 "--owner", "codex", "--purpose", "investigate", "--prompt", "Inspect value.txt without edits."]
for target, expected in (("grok", "grok"), ("agy", "antigravity"), ("lumen", "lumen")):
    request = external_base + ["--to", target, "--model", target + "-fixture",
                               "--task-id", "external-" + target, "--label", "Inspect with " + target]
    planned = json.loads(call(request + ["--dry-run"]))
    assert planned["route"]["provider"] == expected and planned["route"]["effort"] is None
    assert "--reasoning-effort" not in planned["argv"] and planned["executes"] is False
    before = (project / "value.txt").read_bytes()
    env = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])
    answer = call(request, env)
    artifact = re.search(r"^artifact: (.+)$", answer, re.M)
    assert artifact and "inspected the bounded repository task" in Path(artifact.group(1)).read_text(), (target, answer)
    assert (project / "value.txt").read_bytes() == before
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(row for row in attempts if row.get("task_id") == "external-" + target)
    assert attempt["provider"] == expected and attempt["parent_attempt_id"] == main_attempts["codex"]
    assert attempt["refs"]["panel_model"] == target + "-fixture" and attempt["state"] == "done"
    execution = json.loads(worker_log.read_text().splitlines()[-1])
    assert execution["cwd"] != str(project) and execution["parent"] == main_attempts["codex"]
    if target == "lumen":
        assert execution["argv"][0] == "run" and "--access" in execution["argv"]
        assert execution["argv"][execution["argv"].index("--model") + 1] == "lumen-fixture"

for options in (["--to", "lumen"], ["--to", "lumen", "--model", "provider-default"],
                ["--to", "lumen,grok", "--model", "fixture"], ["--to", "unregistered", "--model", "fixture"],
                ["--to", "deepseek", "--model", "fixture"], ["--to", "codex", "--model", "gpt-5.6-sol"],
                ["--to", "grok", "--model", "fixture", "--workload", "deep"],
                ["--model", "fixture"], ["--reasoning-effort", "extreme"],
                ["--to", "lumen", "--model", "fixture", "--access", "write", "--purpose", "implement",
                 "--brief-file", str(brief), "--verify", "test -f value.txt"]):
    before = worker_log.read_bytes()
    failure = subprocess.run(external_base + options + ["--dry-run"], cwd=project,
                             env=environment, capture_output=True, text=True)
    assert failure.returncode != 0, (options, failure.stdout)
    if "--access" in options:
        assert "OMS_PROVIDER_WRITE_ADAPTERS" in failure.stderr, failure.stderr
    assert worker_log.read_bytes() == before
# A role route keeps its tier's model and takes a per-dispatch effort.
harder = json.loads(call(external_base + ["--workload", "light", "--reasoning-effort", "high", "--dry-run"]))
assert harder["route"]["model"] == "claude-haiku-5-5" and harder["route"]["effort"] == "high", harder
assert harder["argv"][harder["argv"].index("--reasoning-effort") + 1] == "high", harder["argv"]
profile_route = json.loads(call(external_base + ["--to", "vibe", "--dry-run"]))
assert profile_route["route"]["model"] == "provider-default" and "--model" not in profile_route["argv"]
answer = call(external_base + ["--to", "vibe", "--task-id", "external-profile", "--label", "Inspect native profile"],
              dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
assert "artifact:" in answer
for role, target in (("advisor", "grok"), ("reviewer", "lumen")):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", role,
               "--owner", "codex", "--to", target, "--model", target + "-fixture", "--prompt", "Inspect source.",
               "--task-id", "external-" + role, "--label", "Inspect with " + role]
    if role == "reviewer":
        request += ["--verify", "test -f value.txt"]
    planned = json.loads(call(request + ["--dry-run"]))
    assert planned["argv"][2] == ("advise" if role == "advisor" else "peer-review")
    if role == "advisor":
        assert planned["argv"][planned["argv"].index("--strategy") + 1] == "advisor", planned["argv"]
    assert "--reasoning-effort" not in planned["argv"]
    call(request, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
opted_in = dict(environment, OMS_PROVIDER_WRITE_ADAPTERS="lumen")
write_request = [a for a in external_base[:external_base.index("--purpose")]] + [
    "--to", "lumen", "--model", "lumen-fixture", "--access", "write", "--purpose", "implement",
    "--brief-file", str(brief), "--verify", 'test "$(tail -1 value.txt)" = new', "--task-id", "external-lumen-write",
    "--label", "Implement with adapter"]
assert json.loads(call(write_request + ["--dry-run"], opted_in))["route"]["access"] == "write"
write_env = dict(opted_in, PANEL_TEST_MODE="write", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])
assert "patch:" in call(write_request, write_env)
assert (project / "value.txt").read_text() == "old\n"
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show", side_effect=empty_control), \
        patch.object(panel, "provider_catalog", return_value=list(registered.values())), \
        patch.object(panel, "dispatch", return_value=0) as routed, \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "read_field", side_effect=["p", "lumen", "5", "lumen-fixture", "", "Inspect source", "", "q"]):
    assert panel.interactive(project) == 0
    assert routed.call_args.kwargs == {"target": "lumen", "model": "lumen-fixture", "effort": None}
snapshot = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--json"]))["dashboard"]
panel_rows = [row for row in snapshot["attempts"]["recent"] if row.get("panel")]
assert {row["panel"]["role"] for row in panel_rows} >= {"worker", "advisor", "reviewer"}, panel_rows
assert all(row.get("parent_attempt_id") in main_attempts.values() for row in panel_rows
           if row["panel"]["role"] != "main")
assert all(row["panel"]["label"] and row["panel"]["access"] for row in panel_rows
           if row["panel"]["role"] != "main")
# An exact native main filters both live children and recent outcomes. Its
# supplied title must never be replaced by an unrelated repository goal.
from copy import deepcopy
from panel_view import render, inbox_items, needs_strip
scoped = deepcopy(snapshot)
scoped["goal"] = {"text": "Unrelated repository goal"}
main_id = main_attempts["claude"]
for row in scoped["attempts"]["active_recent"] + scoped["attempts"]["recent"]:
    if row["attempt_id"] == main_id:
        row["panel"].update(model="claude-opus-5-5", label="Refine panel routing")
own_text = render(scoped, "claude", width=150, height=45, main_attempt=main_id)
assert "Refine panel routing" in own_text and "Unrelated repository goal" not in own_text, own_text
assert "selected: claude-fable-5-1" not in own_text and "selected: claude-sonnet-5-5" not in own_text, own_text
for title in ("".join(("api", "_key", "=", "fixture-value")), "/private/absolute", "../escaping"):
    try:
        panel.safe_label(title)
        raise AssertionError("unsafe task label accepted")
    except ValueError:
        pass
assert panel.safe_label("설명 \033[31m source") == "설명 ?[31m source"

# BEGIN council extra seats
import panel_routing
for model, family, version in (
        ("gemini-4-argon-high", "argon-high", (4,)),
        ("gemini-4.0-argon-medium", "argon-medium", (4,)),
        ("gemini-4.10-argon-low", "argon-low", (4, 10)),
        ("gemini-3.1-pro-high", "pro-high", (3, 1)),
        ("gemini-0-deep-think-high", "deep-think-high", (0,))):
    assert panel.route_family("antigravity", model) == (family, version), model
    assert panel_routing.valid_route({"provider": "antigravity", "model": model, "effort": "high"}), model
for model in (None, "", "gemini-latest", "gemini-4-argon", "gemini-4-argon-ultra",
              "gemini-4..1-argon-high", "gemini-4.-argon-high", "gemini-4--high", "gpt-6-sol"):
    assert panel.route_family("antigravity", model) is None, model
    assert not panel_routing.valid_route({"provider": "antigravity", "model": model, "effort": "high"}), model
configured_extra = panel.policy()["council"]["extra"][0]
assert configured_extra["model"] == "gemini-4-argon-high", configured_extra
argon_catalog = ["gemini-4-argon-" + level for level in ("low", "medium", "high")]
for configured, routable, resolved, note in (
        ("gemini-3.1-argon-high", argon_catalog, "gemini-4-argon-high", ""),
        (configured_extra["model"], argon_catalog, "gemini-4-argon-high", ""),
        (configured_extra["model"], ["gemini-5-argon-high"], "gemini-5-argon-high", ""),
        ("gemini-3.1-pro-high", argon_catalog, "gemini-4-argon-high", ""),
        ("gemini-3.1-pro-high", ["gemini-4-argon-high", "gemini-5-argon-high"], "gemini-5-argon-high", ""),
        ("gemini-3.1-pro-high", ["gemini-4-argon-high", "gemini-4-orion-low"], "gemini-4-argon-high", ""),
        ("gemini-3.1-argon-low", argon_catalog, "gemini-4-argon-low", ""),
        ("gemini-3.1-argon-medium", argon_catalog, "gemini-4-argon-medium", ""),
        ("gemini-4-argon-high", ["gemini-4.2-argon-high", "gemini-4.10-argon-high"], "gemini-4.10-argon-high", ""),
        ("gemini-4.0-argon-high", ["gemini-4-argon-high"], "gemini-4-argon-high", ""),
        ("gemini-3.1-argon-high", ["gemini-4-argon-high", "gemini-4-orion-high"], "gemini-4-argon-high", ""),
        ("gemini-3.1-pro-high", ["gemini-4-argon-high", "gemini-4-orion-high"], None, "configured family pro-high"),
        ("gemini-3.1-argon-high", ["gpt-6-sol"], None, "configured family argon-high"),
        ("gemini-3.1-argon-high", argon_catalog[:2], None, "configured family argon-high"),
        ("gemini-3.1-argon-high", ["gemini-4..1-argon-high", "gemini-5-argon-ultra"], None, "configured family argon-high"),
        ("gemini-latest", argon_catalog, None, "role routes require a versioned model family"),
        ("gemini-3.1-argon-high", [], None, "absent from the catalog"),
        ("gemini-3.1-argon-high", None, None, "absent from the catalog"),
        ("gemini-3.1-argon-high", 0, None, "absent from the catalog"),
        ("gemini-3.1-argon-high", "malformed", None, "absent from the catalog"),
        ("gemini-3.1-argon-high", [None], None, "absent from the catalog")):
    candidate = deepcopy(panel.policy())
    candidate["council"] = {"extra": [{"provider": "antigravity", "model": configured, "effort": "high"}]}
    # Full-catalog presence cannot turn a non-routable model into a seat.
    catalog = [configured, "gemini-3.8-flash-high", "gpt-6-sol"] + (routable if isinstance(routable, list) else [])
    row = {"provider": "antigravity", "installed": True, "models": catalog,
           "catalog_models": catalog, "routable": routable}
    output = io.StringIO()
    with patch.object(panel, "policy", return_value=candidate), \
            patch.object(panel, "provider_catalog", return_value=[row]), redirect_stderr(output):
        argv = panel.council_command(project, "Compare", "extra-seat", "Compare")
    targets = argv[argv.index("--providers") + 1].split(",")
    extra_targets = [target for target in targets if target.startswith("antigravity:")]
    assert extra_targets == (["antigravity:model=" + resolved + ":optional"] if resolved else []), (configured, routable, targets)
    assert note in output.getvalue(), output.getvalue()
    if note.startswith("configured family"):
        assert "routable choices: " + ", ".join(sorted(routable)) in output.getvalue(), output.getvalue()
extra_route = {"provider": "antigravity", "model": "gemini-3.1-pro-high", "effort": "high"}
extra_policy = deepcopy(panel.policy())
extra_policy["council"] = {"extra": [extra_route]}
for installed, models, rounds, workload, included, note in (
        (True, [extra_route["model"]], 1, "deep", True, ""),
        (False, [], 1, "deep", False, "not installed"),
        (True, [], 1, "deep", False, "absent from the catalog"),
        (True, [extra_route["model"]], 2, "deep", False, "5 seats x 3 rounds exceeds the 12-call budget"),
        (True, [extra_route["model"]], 2, "routine", True, "")):
    row = {"provider": "antigravity", "installed": installed, "models": models}
    output = io.StringIO()
    with patch.object(panel, "policy", return_value=extra_policy), \
            patch.object(panel, "provider_catalog", return_value=[row]), redirect_stderr(output):
        argv = panel.council_command(project, "Compare", "extra-seat", "Compare", rounds=rounds, workload=workload)
    assert ("antigravity:model=gemini-3.1-pro-high:optional" in argv[argv.index("--providers") + 1]) == included
    assert note in output.getvalue(), output.getvalue()
for failure in (ValueError("unavailable"), subprocess.TimeoutExpired("catalog", 1), OSError("unavailable")):
    output = io.StringIO()
    with patch.object(panel, "policy", return_value=extra_policy), \
            patch.object(panel, "provider_catalog", side_effect=failure), redirect_stderr(output):
        argv = panel.council_command(project, "Compare", "extra-seat", "Compare")
    assert "antigravity:" not in argv[argv.index("--providers") + 1]
    assert output.getvalue() == "council: antigravity seat skipped: catalog lookup failed\n"
for rows in ([], [{"provider": "other", "installed": False}]):
    output = io.StringIO()
    with patch.object(panel, "policy", return_value=extra_policy), \
            patch.object(panel, "provider_catalog", return_value=rows), redirect_stderr(output):
        argv = panel.council_command(project, "Compare", "extra-seat", "Compare")
    assert "antigravity:" not in argv[argv.index("--providers") + 1]
    assert "not installed" not in output.getvalue()
candidate = deepcopy(extra_policy)
candidate["tiers"]["deep"].append(extra_route)
with patch.object(panel_routing.Path, "read_text", return_value=json.dumps(candidate)):
    try:
        panel_routing.policy()
    except ValueError as error:
        assert "invalid panel tier" in str(error)
    else:
        raise AssertionError("antigravity tier accepted")
for invalid_round in (0, 3, "malformed"):
    try:
        panel.council_command(project, "Compare", "extra-seat", "Compare", rounds=invalid_round)
    except (ValueError, TypeError):
        pass
    else:
        raise AssertionError("invalid council rounds accepted")
for council_value in (None, {}, {"extra": None}, {"extra": [extra_route, extra_route]},
                      {"extra": [dict(extra_route, model="")]}, {"extra": [dict(extra_route, effort="bad")]}):
    candidate = deepcopy(extra_policy)
    candidate["council"] = council_value
    with patch.object(panel_routing.Path, "read_text", return_value=json.dumps(candidate)):
        try:
            panel_routing.policy()
        except ValueError:
            pass
        else:
            raise AssertionError("invalid extra routes accepted")
for council_value in (None, {"extra": []}, {"extra": [extra_route]}):
    candidate = deepcopy(extra_policy)
    if council_value is None:
        del candidate["council"]
    else:
        candidate["council"] = council_value
    with patch.object(panel_routing.Path, "read_text", return_value=json.dumps(candidate)):
        assert panel_routing.policy()["council"]["extra"] == (council_value or {"extra": []})["extra"]
budget_policy = deepcopy(extra_policy)
budget_policy["tiers"]["deep"] = budget_policy["tiers"]["deep"][:1]
with patch.object(panel, "policy", return_value=budget_policy), \
        patch.object(panel, "provider_catalog", return_value=[{"provider": "antigravity", "installed": True,
                                                             "routable": [extra_route["model"]]}]):
    argv = panel.council_command(project, "Compare", "extra-seat", "Compare", rounds=2)
assert len(argv[argv.index("--providers") + 1].split(",")) * 3 == 12, argv
class CouncilStarted(Exception):
    pass
advertised = io.StringIO()
with patch.object(panel, "policy", return_value=extra_policy), \
        patch.object(panel, "provider_catalog", return_value=[{"provider": "antigravity", "installed": True, "routable": argon_catalog}]), \
        patch.object(panel, "child_environment", return_value={}), \
        patch.object(panel.room, "resolve", return_value=None), \
        patch.object(panel, "events", side_effect=CouncilStarted), redirect_stdout(advertised):
    try:
        panel.council(project, "codex", "extra-seat", "Compare", "Compare")
    except CouncilStarted:
        pass
    else:
        raise AssertionError("council did not start")
assert "gemini-4-argon-high" in advertised.getvalue(), advertised.getvalue()
assert extra_route["model"] not in advertised.getvalue(), advertised.getvalue()
assert ":optional" not in advertised.getvalue(), advertised.getvalue()
# END council extra seats

# BEGIN council prompt defaults
base = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--council", "--owner", "codex", "--dry-run"]
for prompt, label in (("  Compare options  \nDetails", "Compare options"),
                      ("x" * 60, "x" * 60), ("x" * 61, "x" * 60 + "…")):
    derived = subprocess.run(base + ["--prompt", prompt], cwd=str(project), env=environment,
                             capture_output=True, text=True)
    assert derived.returncode == 0, derived.stderr
    plan = json.loads(derived.stdout)
    ident = "council-" + hashlib.sha256(prompt.encode("utf-8")).hexdigest()[:8]
    assert (plan["task_id"], plan["label"]) == (ident, label), plan
    assert "task-id=" + ident in derived.stderr and "label=" + json.dumps(label, ensure_ascii=True) in derived.stderr, derived.stderr
    for options, expected_id, expected_label in ((["--task-id", "chosen"], "chosen", label),
                                                  (["--label", "Chosen"], ident, "Chosen"),
                                                  (["--task-id", "chosen", "--label", "Chosen"], "chosen", "Chosen")):
        result = subprocess.run(base + ["--prompt", prompt] + options, cwd=str(project), env=environment,
                                capture_output=True, text=True)
        assert result.returncode == 0, result.stderr
        plan = json.loads(result.stdout)
        assert (plan["task_id"], plan["label"]) == (expected_id, expected_label), plan
        assert ("council defaults:" in result.stderr) == (len(options) < 4), result.stderr
for options in ([], ["--prompt", ""], ["--prompt", "\n"],
                ["--prompt", "valid", "--task-id", "bad/id"], ["--prompt", "valid", "--label", ""]):
    result = subprocess.run(base + options, cwd=str(project), env=environment, capture_output=True, text=True)
    assert result.returncode != 0, result
# END council prompt defaults

# BEGIN council lens seats
lens_args = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--council",
             "--owner", "codex", "--task-id", "lens-council", "--label", "Lens review",
             "--prompt", "Compare options.", "--workload", "routine"]
lens_env = dict(environment, OMS_RESUME_SESSION="old-session", OMS_PANEL_FALLBACK_PROVIDER="codex",
                OMS_PANEL_FALLBACK_MODEL="gpt-6-luna", OMS_PANEL_FALLBACK_EFFORT="low")
for key in ("OMS_ROOM_ID", "OMS_ROOM_REPO", "OMS_ROOM_PARTICIPANT", "OMS_PANEL_MAIN_ATTEMPT",
            "OMS_COUNCIL_LENSES"):
    lens_env.pop(key, None)
(capability_dir / "antigravity.models").write_text("fixture-unrouted\n")
default_plan = json.loads(call(lens_args + ["--dry-run"], lens_env))
assert default_plan["primary_calls"] == 4 and default_plan["seats"] == 2
for size in (2, 3, 12):
    names = ",".join("lens-%s" % n for n in range(size))
    plan = json.loads(call(lens_args + ["--lenses", names, "--dry-run"], lens_env))
    assert (plan["seats"], plan["families"], plan["primary_calls"]) == (size, 2, size), plan
    argv = plan["argv"]
    assert argv[argv.index("--debate") + 1] == "0", argv
    targets = argv[argv.index("--providers") + 1].split(",")
    assert targets == ["claude:model=claude-sonnet-5-5", "codex:model=gpt-6.1-terra"] * (size // 2) + (
        ["claude:model=claude-sonnet-5-5"] if size % 2 else []), targets
boundary_plan = json.loads(call(lens_args + ["--lenses", "a," + "b" * 32, "--dry-run"], lens_env))
assert boundary_plan["seats"] == boundary_plan["primary_calls"] == 2
for tier in ("deep", "frontier"):
    base_plan = json.loads(call(lens_args + ["--workload", tier, "--dry-run"], lens_env))
    lens_plan = json.loads(call(lens_args + ["--workload", tier, "--lenses", "a,b,c,d,e", "--dry-run"], lens_env))
    base_argv, lens_argv = base_plan["argv"], lens_plan["argv"]
    base_targets = base_argv[base_argv.index("--providers") + 1].split(",")
    assert lens_argv[lens_argv.index("--providers") + 1].split(",") == base_targets + base_targets[:1]
extra_policy = deepcopy(panel.policy())
extra_policy["council"] = {"extra": [{"provider": "antigravity", "model": "gemini-3.1-pro-high", "effort": "high"}]}
with patch.object(panel, "policy", return_value=extra_policy), patch.object(panel, "provider_catalog", return_value=[
        {"provider": "antigravity", "installed": True, "models": ["gemini-3.1-pro-high"]}]):
    argv = panel.council_command(project, "Compare", "lens", "Lens", workload="routine", lenses="a,b,c,d,e")
    targets = argv[argv.index("--providers") + 1].split(",")
    assert targets == ["claude:model=claude-sonnet-5-5", "codex:model=gpt-6.1-terra",
                       "antigravity:model=gemini-3.1-pro-high:optional",
                       "claude:model=claude-sonnet-5-5", "codex:model=gpt-6.1-terra"], targets
before_calls = worker_log.read_bytes() if worker_log.exists() else b""
attempt_log = project / ".oms/lifecycle/events.jsonl"
before_attempts = attempt_log.read_bytes() if attempt_log.exists() else b""
for options, message in ((["--lenses", ""], "lenses"), (["--lenses", "one"], "2..12"),
                         (["--lenses", "a," + "b" * 33], "32"),
                         (["--lenses", ",".join("lens-%s" % n for n in range(13))], "2..12"),
                         (["--lenses", "one,one"], "unique"),
                         (["--lenses", "one,UPPER"], "[a-z0-9-]+"),
                         (["--lenses", "one,,two"], "[a-z0-9-]+"),
                         (["--lenses", "one,two,"], "[a-z0-9-]+"),
                         (["--lenses", "one,two", "--rounds", "1"], "--rounds 0"),
                         (["--lenses", "one,two", "--rounds", "bad"], "invalid int")):
    result = subprocess.run(lens_args + options, env=lens_env, capture_output=True, text=True,
                            timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
    assert result.returncode != 0 and message in result.stderr, result
assert (worker_log.read_bytes() if worker_log.exists() else b"") == before_calls
assert (attempt_log.read_bytes() if attempt_log.exists() else b"") == before_attempts
single_family = deepcopy(panel.policy())
single_family["tiers"]["routine"] = single_family["tiers"]["routine"][:1]
with patch.object(panel, "policy", return_value=single_family):
    try:
        panel.council_command(project, "Compare", "lens", "Lens", workload="routine", lenses="one,two")
        raise AssertionError("one-family lens council accepted")
    except ValueError as error:
        assert "two independent provider families" in str(error), error
for options in (["--lenses", "one,two"], ["--rounds", "1"]):
    no_council = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project)] + options,
                                env=lens_env, capture_output=True, text=True)
    assert no_council.returncode != 0 and "--council" in no_council.stderr
duplicate = subprocess.run(["bash", str(panel.ENTRY), "peer-ask", "--repo", str(project),
                            "--providers", "codex,codex", "--prompt", "Compare"],
                           env=lens_env, capture_output=True, text=True)
assert duplicate.returncode != 0 and "duplicate target" in duplicate.stderr, duplicate
internal_peer = ["bash", str(panel.ENTRY), "peer-ask", "--repo", str(project), "--prompt", "Compare",
                 "--providers", "claude,codex", "--debate", "0", "--deliberation"]
for value, options in (("", []), ("one", []), ("one,two,three", []), ("one,one", []),
                       ("a," + "b" * 33, []), ("one,two,three", ["--providers", "claude,,codex"]),
                       ("one,two", ["--providers", "routine"]),
                       ("one,UPPER", []), ("one,two", ["--debate", "1"]),
                       ("one,two", ["--export-only"]), ("one,two", ["--dry-run"])):
    rejected = subprocess.run(internal_peer + options, env=dict(lens_env, OMS_COUNCIL_LENSES=value),
                              capture_output=True, text=True)
    assert rejected.returncode != 0, rejected
assert (worker_log.read_bytes() if worker_log.exists() else b"") == before_calls
assert (attempt_log.read_bytes() if attempt_log.exists() else b"") == before_attempts
threads = []
for run_lenses in (("correctness", "security", "compatibility"), ("correctness", "security", "c" * 32)):
    output = call(lens_args + ["--lenses", ",".join(run_lenses), "--rounds", "0",
                              "--thread", "lens-prefix"], lens_env)
    thread_id = next(line.removeprefix("thread: ") for line in output.splitlines() if line.startswith("thread: "))
    assert thread_id.startswith("lens-prefix-") and thread_id not in threads, output
    threads.append(thread_id)
    rows = [json.loads(line) for line in (project / ".oms/threads" / (thread_id + ".jsonl")).read_text().splitlines()]
    answers = [row for row in rows if row["role"] == "answer"]
    assert len(answers) == 3, rows
    paths = [Path(row["artifact"]) for row in answers]
    paths = [path if path.is_absolute() else project / path for path in paths]
    assert len(set(paths)) == 3, paths
    for index, lens in enumerate(run_lenses, 1):
        seat = "seat-%02d" % index
        path = next(path for path in paths if seat + "-" + lens in path.name)
        prompt = path.read_text().split("## Prompt\n", 1)[1].split("## Output\n", 1)[0]
        assert "Review only through the %s lens" % lens in prompt, prompt
        assert "Evidence:" in prompt and "Counterargument:" in prompt, prompt
        assert "inspected source" not in prompt and "Prior thread" not in prompt, prompt
        turn = next(row for row in answers if seat in row["text"])
        assert lens in turn["text"] and ("claude:model=" if index % 2 else "codex:model=") in turn["text"], turn
    synthesis_path = Path(next(line.removeprefix("synthesis: ") for line in output.splitlines() if line.startswith("synthesis: ")))
    synthesis = synthesis_path.read_text()
    assert len(synthesis_path.read_bytes()) <= 65536
    assert all("seat-%02d" % n in synthesis and lens in synthesis for n, lens in enumerate(run_lenses, 1)), synthesis
lens_binary = temporary / "lens-bin"
lens_binary.mkdir()
lens_calls = temporary / "lens-calls.jsonl"
for provider in ("claude", "codex"):
    executable = lens_binary / provider
    executable.write_text('''#!/usr/bin/env python3
import json, os, sys
assert "resume" not in sys.argv and "--resume" not in sys.argv, sys.argv
if "--help" in sys.argv or "--version" in sys.argv:
    print("fixture --effort low medium high --config")
    raise SystemExit(0)
prompt = sys.stdin.read()
with open(os.environ["LENS_CALLS"], "a") as stream:
    stream.write(json.dumps(prompt) + "\\n")
if "lens: security," in prompt or (os.environ.get("LENS_FORCE_FALLBACK") and "gpt-6.1-terra" in sys.argv):
    print("selected model is at capacity")
    raise SystemExit(7)
sections = ["Answer", "Alternatives", "Evidence", "Counterargument", "Risks", "Recommendation",
            "Verification", "Changed from previous round", "Remaining disagreements"]
if "lens: compatibility," in prompt and "Deliberation repair:" not in prompt:
    sections.remove("Evidence")
for section in sections:
    print(section + ": retained fixture conclusion.")
''')
    executable.chmod(0o755)
failed_run = subprocess.run(lens_args + ["--lenses", "correctness,security,compatibility"],
                            env=dict(lens_env, PATH=str(lens_binary) + os.pathsep + lens_env["PATH"],
                                     LENS_CALLS=str(lens_calls)), capture_output=True, text=True,
                            timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
assert failed_run.returncode != 0, failed_run
failures = [json.loads(line) for line in (project / ".oms/failures.jsonl").read_text().splitlines()]
security_failure = next(row for row in reversed(failures) if "lens: security," in row.get("summary", ""))
assert security_failure["cmd"] == "peer-ask seat codex:model=gpt-6.1-terra", security_failure
assert "seat-02" in security_failure["summary"], security_failure
prompts = [json.loads(line) for line in lens_calls.read_text().splitlines()]
assert len(prompts) == 4, prompts
repair_prompt = next(prompt for prompt in prompts if "Deliberation repair:" in prompt)
assert "seat-03 (lens: compatibility, target: claude:model=claude-sonnet-5-5)" in repair_prompt
assert "Review only through the compatibility lens" in repair_prompt
synthesis_path = Path(next(line.removeprefix("synthesis: ") for line in failed_run.stdout.splitlines()
                           if line.startswith("synthesis: ")))
synthesis = synthesis_path.read_text()
assert "seat-02 (lens: security, target: codex:model=gpt-6.1-terra)" in synthesis and "no answer:" in synthesis
thread_id = next(line.removeprefix("thread: ") for line in failed_run.stdout.splitlines() if line.startswith("thread: "))
rows = [json.loads(line) for line in (project / ".oms/threads" / (thread_id + ".jsonl")).read_text().splitlines()]
assert any(row["role"] == "note" and "seat-02" in row["text"] and "security" in row["text"] for row in rows)
repaired = next(row for row in rows if row["role"] == "answer" and "seat-03" in row["text"])
assert "seat-03-compatibility" in repaired["artifact"] and repaired["artifact"].endswith("-repair.md"), repaired
assert "seat-02" in failed_run.stderr and "security" in failed_run.stderr, failed_run
check_history = ["bash", str(panel.ENTRY), "fail-ledger", "--repo", str(project), "check", "--ignore-state",
                 "--cmd", "peer-ask seat codex:model=gpt-6.1-terra"]
unresolved = subprocess.run(check_history, env=lens_env, capture_output=True, text=True)
assert unresolved.returncode == 3, unresolved
fallback_run = subprocess.run(["bash", str(panel.ENTRY), "peer-ask", "--repo", str(project),
                               "--providers", "codex:model=gpt-6.1-terra", "--fallback-model", "gpt-6-luna",
                               "--prompt", "Compare options."],
                              env=dict(lens_env, PATH=str(lens_binary) + os.pathsep + lens_env["PATH"],
                                       LENS_CALLS=str(lens_calls), LENS_FORCE_FALLBACK="1", OMS_RESUME_SESSION=""),
                              capture_output=True, text=True,
                              timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
assert fallback_run.returncode == 0, fallback_run
still_unresolved = subprocess.run(check_history, env=lens_env, capture_output=True, text=True)
assert still_unresolved.returncode == 3, still_unresolved
recovered_run = subprocess.run(lens_args, env=lens_env, capture_output=True, text=True,
                               timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
assert recovered_run.returncode == 0, recovered_run
assert "codex:model=gpt-6.1-terra ask seat has unresolved no-answer history" in recovered_run.stderr, recovered_run
recovered = subprocess.run(check_history, env=lens_env, capture_output=True, text=True)
assert recovered.returncode == 0, recovered
# END council lens seats

# Keep the subprocess council independent of any installed provider catalog.
(capability_dir / "antigravity.models").write_text("fixture-unrouted\n")
# The council reuses the peer front door: seats over two families, explicit rounds,
# bounded repairs, and the owning main's child links rather than a new task DB.
council_args = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--council",
                "--owner", "codex", "--task-id", "panel-council-test", "--label", "Decide readable results",
                "--prompt", "Inspect value.txt and compare the result presentation."]
before_index = (project / ".oms/artifacts/index.jsonl").read_bytes()
for rounds in (1, 2):
    planned = json.loads(call(council_args + ["--rounds", str(rounds), "--dry-run"]))
    assert planned["primary_calls"] == 4 * (rounds + 1) and planned["max_calls"] == 8 * (rounds + 1)
    assert planned["families"] == 2 and planned["executes"] is False
    assert planned["argv"][planned["argv"].index("--providers") + 1] == \
        "claude:model=claude-opus-5-5,codex:model=gpt-6.1-sol,claude:model=claude-sonnet-5-5,codex:model=gpt-6.1-terra"
    assert len(planned["argv"][planned["argv"].index("--providers") + 1].split(",")) == 4 == planned["seats"]
for rounds in (1, 2):
    routine_plan = json.loads(call(council_args + ["--workload", "routine", "--rounds", str(rounds), "--dry-run"]))
    assert (routine_plan["seats"], routine_plan["families"], routine_plan["primary_calls"]) == (2, 2, 2 * (rounds + 1))
    assert routine_plan["argv"][routine_plan["argv"].index("--providers") + 1] == \
        "claude:model=claude-sonnet-5-5,codex:model=gpt-6.1-terra"
for invalid_tier in ("", "light", "malformed"):
    try:
        panel.council_command(project, "Compare", "invalid-tier", "Compare", workload=invalid_tier)
    except ValueError:
        pass
    else:
        raise AssertionError("invalid council tier accepted: " + invalid_tier)
frontier_plan = json.loads(call(council_args + ["--workload", "frontier", "--dry-run"]))
assert frontier_plan["argv"][frontier_plan["argv"].index("--providers") + 1] == \
    "claude:model=claude-opus-5-5,codex:model=gpt-6.1-sol,claude:model=claude-fable-5-1,codex:model=gpt-6-astra"
assert (project / ".oms/artifacts/index.jsonl").read_bytes() == before_index
# A council rejects a tier that no longer preserves two-family independence.
one_route = deepcopy(panel.policy())
one_route["tiers"]["deep"] = one_route["tiers"]["deep"][:1]
one_route["tiers"]["routine"] = one_route["tiers"]["routine"][:1]
with patch.object(panel, "policy", return_value=one_route):
    for tier in ("routine", "deep"):
        try:
            panel.council(project, "codex", "council-one-route", "One route", "Compare options.", dry_run=True, workload=tier)
        except ValueError as error:
            assert "two independent provider families" in str(error)
        else:
            raise AssertionError("council started with one provider family")
call(council_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
council_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
council_attempts = [a for a in council_attempts if a.get("task_id") == "panel-council-test"]
assert any(a["tool"] == "panel-council" and a["state"] == "done" for a in council_attempts)
assert {a["refs"].get("panel_model") for a in council_attempts if a["tool"] != "panel-council"} == \
    {"gpt-6.1-sol", "claude-opus-5-5", "claude-sonnet-5-5", "gpt-6.1-terra"}, council_attempts
assert all(a.get("parent_attempt_id") == main_attempts["codex"] for a in council_attempts)
# A control-menu council has no inherited main: it binds the room's one live main, or refuses, so Debate can show it.
class CouncilBound(Exception):
    pass
def council_events(repo, *args, **kwargs):
    if args[0] == "show":
        return json.dumps({"tool": "panel-main", "provider": "codex", "terminal": False})
    raise CouncilBound(list(args))
menu_env = {"OMS_ROOM_ID": "council-room", "OMS_ROOM_PARTICIPANT": ""}
with patch.dict(os.environ, menu_env), patch.object(panel.room, "records", return_value=[]), \
        patch.object(panel.room, "project", return_value={}), patch.object(panel, "events", council_events), \
        patch.object(panel, "active_mains", return_value={"m1": {"attempt_id": "att_m1"}}):
    try:
        panel.council(project, "codex", "menu-council", "Menu council", "Compare options.")
    except CouncilBound as bound:
        assert "--parent-attempt-id" in bound.args[0] and "att_m1" in bound.args[0], bound.args
        assert "panel_room_id=council-room" in bound.args[0], bound.args
    else:
        raise AssertionError("council did not start")
for none in ({}, {"m1": {"attempt_id": "a1"}, "m2": {"attempt_id": "a2"}}):
    with patch.dict(os.environ, menu_env), patch.object(panel.room, "records", return_value=[]), \
            patch.object(panel.room, "project", return_value={}), patch.object(panel, "events", council_events), \
            patch.object(panel, "active_mains", return_value=none):
        try:
            panel.council(project, "codex", "menu-council", "Menu council", "Compare options.")
        except ValueError as refused:
            assert "unique active main" in str(refused)
        else:
            raise AssertionError("a council without a unique main started")

import panel_results as saved
def panel_with_review_wait(arguments, seconds, **options):
    """Run the panel CLI with the finalize review-wait bound patched (no environment knob)."""
    code = ("import sys, panel_results, terminal_panel; panel_results.REVIEW_WAIT_SECONDS = %s; "
            "sys.exit(terminal_panel.main(sys.argv[1:]))" % seconds)
    return subprocess.run([sys.executable, "-c", code, *arguments], capture_output=True, text=True,
                          env=dict(options.pop("env"), PYTHONPATH=str(root / "scripts/lib")), **options)
from panel_view import render_results
from types import SimpleNamespace

# Settled delegate artifacts carry complete bounded findings across task IDs.
carry_attempts = []
carry_rows = []
carry_payloads = {}
carry_repo = Path.home() / "fixture-repo"
for source_task, artifact, body in (
        ("source-task", ".oms/artifacts/critic.md",
         '## Output\n## Findings\n[P1] Fix `"%s/value.py:42"`\nDetails retained.\n## Exit\n0\n' % carry_repo),
        ("secret-task", ".oms/artifacts/security.md",
         "## Output\n## Findings\n[P1] api" "_key=fixture-secret-shaped-value\n## Exit\n0\n")):
    ident = "attempt-" + source_task
    carry_attempts.append({"attempt_id": ident, "task_id": source_task, "state": "done",
                           "refs": {"panel_access": "read", "panel_role": "critic"}})
    row = {"attempt_id": ident, "task_id": source_task, "kind": "delegate", "exit": 0,
           "artifact": artifact, "outcome": "completed", "ts": source_task}
    carry_rows.append(row)
    carry_payloads[artifact] = ("## Output\n" + body.split("## Output\n", 1)[1]).encode()
with patch.object(saved, "_records", return_value=(carry_attempts, carry_rows)), \
        patch.object(saved, "_indexed", side_effect=lambda repo, row, limit: carry_payloads[row["artifact"]]):
    same_task_carry = saved.prior_findings(carry_repo, "source-task", "")
    cross_task_carry = saved.prior_findings(carry_repo, ["destination-task", "source-task"], "")
    secret_carry = saved.prior_findings(carry_repo, ["secret-task"], "")
assert "critic" in same_task_carry and ".oms/artifacts/critic.md" in same_task_carry
assert '"value.py:42"' in same_task_carry and "Details retained." in same_task_carry
assert "source-task" in cross_task_carry
assert "finding omitted (sensitive-looking content; read .oms/artifacts/security.md)" in secret_carry
assert "fixture-secret-shaped-value" not in secret_carry
assert saved._scan_finding_blocks([]) == []
finding_line = carry_payloads[".oms/artifacts/security.md"].decode().splitlines()[2]
with patch.object(saved.subprocess, "run", wraps=subprocess.run) as scan_call:
    assert saved._scan_finding_blocks(["safe", finding_line, "x" * 2400, "x" * 2400 + "\n" + finding_line, "safe again"]) == [True, False, True, False, True]
    assert scan_call.call_count == 1
for result in (SimpleNamespace(returncode=1, stdout="0\n"),
               SimpleNamespace(returncode=0, stdout="malformed\n"),
               SimpleNamespace(returncode=0, stdout="")):
    with patch.object(saved.subprocess, "run", return_value=result):
        assert saved._scan_finding_blocks(["safe"]) == [False]
with patch.object(saved.subprocess, "run", side_effect=OSError("unavailable")):
    assert saved._scan_finding_blocks(["safe"]) == [False]
    assert saved._contains_outbound_machine_path("safe")
with patch.object(saved.subprocess, "run", side_effect=subprocess.TimeoutExpired("scanner", 30)):
    assert saved._scan_finding_blocks(["safe"]) == [False]
with patch.dict(os.environ, {"FIXTURE_" + "TO" + "KEN": "distinct-example-817263"}):
    assert saved._scan_finding_blocks(["distinct-example-817263"]) == [False]
with patch.object(saved, "_records", return_value=(carry_attempts, carry_rows)), \
        patch.object(saved, "_indexed", side_effect=lambda repo, row, limit: carry_payloads[row["artifact"]]), \
        patch.object(saved, "_contains_outbound_machine_path", return_value=False), \
        patch.object(saved.subprocess, "run", side_effect=OSError("unavailable")):
    failed_scan_carry = saved.prior_findings(carry_repo, carry_attempts[1]["task_id"], "")
assert "finding omitted (sensitive-looking content; read .oms/artifacts/security.md)" in failed_scan_carry
assert "fixture-secret-shaped-value" not in failed_scan_carry
from panel_review import _relativize_quoted_paths
escaped_path = str(carry_repo) + "/../../outside/private.txt:42"
assert _relativize_quoted_paths('`%s`' % escaped_path, [str(carry_repo)]) == '`%s`' % escaped_path
if True:
    link_dir = temporary / "relativize-symlink"
    link_root = link_dir / "repo"
    link_root.mkdir(parents=True)
    (link_dir / "outside.py").write_text("x\n")
    (link_root / "link.py").symlink_to(link_dir / "outside.py")
    for anchor in ("", ":42", ":42:7"):
        cited = "`%s/link.py%s`" % (link_root, anchor)
        assert _relativize_quoted_paths(cited, [str(link_root)]) == cited, anchor
    (link_root / "inside.py").write_text("x\n")
    assert _relativize_quoted_paths("`%s/inside.py:42`" % link_root, [str(link_root)]) == "`inside.py:42`"
unc_path = "\\\\server\\share\\private.txt:42"
unc_artifact = ("## Output\n## Findings\n[P1] inspect %s\n## Exit\n0\n" % unc_path).encode()
unc_row = dict(carry_rows[0], artifact_sha256="")
with patch.object(saved, "_records", return_value=(carry_attempts[:1], [unc_row])), \
        patch.object(saved, "_indexed", return_value=unc_artifact):
    unc_carry = saved.prior_findings(carry_repo, "source-task", "")
assert "summary omitted (contains a machine path)" in unc_carry
brief_path = temporary / "carry-secret-brief.md"
brief_path.write_text("Initial scope.\n" + secret_carry, encoding="utf-8")
dispatch._validate_dry_run_inputs(None, brief_path)
brief_path.write_text("Initial scope.\n" + carry_payloads[".oms/artifacts/security.md"].decode().splitlines()[2] + "\n", encoding="utf-8")
try:
    dispatch._validate_dry_run_inputs(None, brief_path)
except ValueError as error:
    assert "sensitive-looking" in str(error), error
else:
    raise AssertionError("main-written sensitive content passed outbound scan")

# BEGIN finalized survey next-ready projection
import hook_state
survey_repo = temporary / "survey-ready"
survey_plan_path = survey_repo / ".oms/plan/tasks.json"
survey_plan_path.parent.mkdir(parents=True)
survey_stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
survey_plan = {"plan_id": "survey-plan", "goal": "Inspect then implement", "tasks": {
    "survey": {"id": "survey", "title": "Read-only survey", "state": "ready", "lease_id": "",
               "created": "1", "updated": survey_stamp},
    "dependent": {"id": "dependent", "state": "ready", "created": "2", "depends": ["survey"]},
    "next-task": {"id": "next-task", "title": "Next eligible work", "state": "ready", "created": "3"}}}
survey_plan_path.write_text(json.dumps(survey_plan))
survey_binding = saved._plan_bindings(survey_repo, {"survey"})["survey"]
survey_result = {"schema": 1, "kind": "oms-panel-result", "task_id": "survey", "owner": "codex",
                 "revision": "1" * 24, "outcome": "completed", "summary": "Survey answered",
                 "evidence": [], "verification": {"status": "not_run"}, "plan_binding": survey_binding}
survey_rows = []
def survey_record(value):
    relative = ".oms/artifacts/panel-results/survey-%s.json" % hashlib.sha256(
        json.dumps(value, sort_keys=True).encode()).hexdigest()[:24]
    result_path = Path(saved._write(survey_repo, relative, value))
    survey_rows[:] = [{"kind": "panel-result", "task_id": "survey", "artifact": relative,
                      "artifact_sha256": hashlib.sha256(result_path.read_bytes()).hexdigest()}]
def survey_hint(expected):
    before = survey_plan_path.read_bytes()
    notices = io.StringIO()
    with patch.object(saved, "_records", return_value=([], survey_rows)), redirect_stderr(notices):
        panel._print_dispatch_next_ready(survey_repo, "codex")
        with patch.dict(os.environ, OMS_ROOM_REPO=str(survey_repo), OMS_ROOM_PARTICIPANT=""):
            context = hook_state.panel_plan_hint({"cwd": str(project)})
    assert "next ready: " + expected in notices.getvalue(), notices.getvalue()
    assert "next ready: " + expected in context, context
    assert "0 of 3 verified" in context, context
    assert survey_plan_path.read_bytes() == before, "hint changed plan state"
    assert "next ready: dependent" not in notices.getvalue(), notices.getvalue()
survey_hint("survey")
for survey_outcome in ("completed", "accepted", "failed", "rejected"):
    survey_record(dict(survey_result, outcome=survey_outcome))
    survey_hint("next-task" if survey_outcome in ("completed", "accepted") else "survey")
survey_record(survey_result)
# A later failed decision must not inherit an earlier successful answer.
successful_row = dict(survey_rows[0])
successful_row["artifact"] = ".oms/artifacts/panel-results/success.json"
saved._write(survey_repo, successful_row["artifact"], survey_result)
survey_record(dict(survey_result, outcome="failed"))
survey_rows.insert(0, successful_row)
survey_hint("survey")
survey_record(survey_result)
survey_plan["tasks"]["survey"]["title"] = "Amended within the same timestamp"
survey_plan_path.write_text(json.dumps(survey_plan))
survey_hint("survey")
survey_plan["tasks"]["survey"]["title"] = "Read-only survey"
survey_plan["tasks"]["survey"]["updated"] = "2099-01-01T00:00:00Z"
survey_plan_path.write_text(json.dumps(survey_plan))
survey_hint("survey")
survey_plan["tasks"]["survey"]["updated"] = survey_stamp
survey_plan["plan_id"] = "replacement-plan"
survey_plan_path.write_text(json.dumps(survey_plan))
survey_hint("survey")
survey_plan["plan_id"] = "survey-plan"
survey_plan_path.write_text(json.dumps(survey_plan))
del survey_plan["plan_id"]
survey_plan_path.write_text(json.dumps(survey_plan))
survey_hint("survey")
survey_plan["plan_id"] = "survey-plan"
survey_plan_path.write_text(json.dumps(survey_plan))
for bad_binding in (None, {}, {"plan_id": "survey-plan", "task_sha256": "0" * 64}):
    survey_record(dict(survey_result, plan_binding=bad_binding))
    survey_hint("survey")
survey_record(dict(survey_result, outcome="malformed"))
survey_hint("survey")
survey_rows[0]["artifact_sha256"] = "0" * 64
survey_hint("survey")
with patch.object(saved, "_records", side_effect=ValueError("unreadable index")):
    assert "next ready: survey" in hook_state.panel_next_ready_hint(hook_state.plan_tasks(survey_repo)[1], "codex")
survey_empty_repo = temporary / "empty-survey-plan"
survey_empty_repo.mkdir()
with patch.object(saved, "_records") as unused_records:
    assert hook_state.plan_tasks(survey_empty_repo) == ({}, [])
    assert saved.outcomes(survey_repo, [], _current_only=True) == {}
    unused_records.assert_not_called()
survey_record(survey_result)
del survey_plan["tasks"]["next-task"]
survey_plan_path.write_text(json.dumps(survey_plan))
with patch.object(saved, "_records", return_value=([], survey_rows)):
    assert not hook_state.panel_next_ready_hint(hook_state.plan_tasks(survey_repo)[1], "codex")
# END finalized survey next-ready projection

# BEGIN finalized survey plan hint
for has_write_patch, shown_task, expected in (
        (True, {"state": "review"}, "oms panel --accept hint-task, then oms panel --finalize --landed-commit SHA"),
        (False, {"state": "ready", "lease_id": ""}, "oms agent-plan cancel --id hint-task"),
        (False, {"state": "ready"}, "oms agent-plan cancel --id hint-task"),
        (False, {"state": "ready", "lease_id": None}, "oms agent-plan cancel --id hint-task"),
        (False, {"state": "ready", "lease_id": 0}, "oms agent-plan cancel --id hint-task"),
        (False, {"state": "ready", "lease_id": "lease"}, "oms agent-plan block --id hint-task --reason TEXT"),
        (False, {"state": "claimed", "lease_id": "lease"}, "oms agent-plan accept-main-research (completion bundle)"),
        (False, {"state": "running", "lease_id": "lease"}, "oms agent-plan accept-main-research (completion bundle)"),
        (False, {"state": "review", "lease_id": "lease"}, "oms agent-plan accept-main-research (completion bundle)"),
        (False, {"state": "review"}, "oms agent-plan accept-main-research (completion bundle)"),
        (False, {"state": "blocked", "lease_id": "lease"}, "oms agent-plan accept-main-research (completion bundle)"),
        (False, {"state": "done"}, "")):
    hint = io.StringIO()
    with patch.object(saved.subprocess, "run", return_value=SimpleNamespace(
            returncode=0, stdout=json.dumps(shown_task), stderr="")), redirect_stderr(hint):
        saved._plan_hint(project, "hint-task", has_write_patch)
    assert expected in hint.getvalue(), hint.getvalue()
    if not expected:
        assert not hint.getvalue(), hint.getvalue()
    if "cancel --id" in expected:
        for flag in ("--expected-plan-sha256 SHA", "--expected-task-sha256 SHA", "--expected-state ready",
                     "--expected-lease-id none", "--operator-consent cancel", "--reason TEXT",
                     "--decision-artifact ARTIFACT", "--expected-decision-artifact-sha256 SHA"):
            assert flag in hint.getvalue(), hint.getvalue()
        assert "accept-main-research" not in hint.getvalue(), hint.getvalue()
        assert len(hint.getvalue().splitlines()) == 1, hint.getvalue()
    if not has_write_patch:
        assert "after oms land" not in hint.getvalue(), hint.getvalue()
for unavailable in (SimpleNamespace(returncode=1, stdout="", stderr="missing task"),
                    *(SimpleNamespace(returncode=0, stdout=body, stderr="")
                      for body in ("", "malformed", "null", "[]", "0", "{}"))):
    hint = io.StringIO()
    with patch.object(saved.subprocess, "run", return_value=unavailable), redirect_stderr(hint):
        saved._plan_hint(project, "hint-task", False)
    assert not hint.getvalue(), hint.getvalue()
# END finalized survey plan hint
first = json.loads(call(final_args))
assert first["verification"]["status"] == "passed"
# A result recorded without panel identity still reaches the shared Plan,
# while an old generation, reopened task or changed proof cannot settle it.
plan_path = project / ".oms/plan/tasks.json"
plan_path.parent.mkdir(parents=True, exist_ok=True)
before_plan = plan_path.read_bytes() if plan_path.exists() else None
external_plan = {"plan_id": "plan_elsewhere", "goal": "Elsewhere work", "tasks": {
    "elsewhere-task": {"id": "elsewhere-task", "title": "Completed outside the panel", "state": "review",
                       "verify": "test -f value.txt", "updated": "2026-10-10T00:00:00Z"}}}
def write_external_plan():
    plan_path.write_text(json.dumps(external_plan), encoding="utf-8")
write_external_plan()
outside_args = list(final_args)
outside_args[outside_args.index("--task-id") + 1] = "elsewhere-task"
outside_args[outside_args.index("--outcome") + 1] = "completed"
other_directory = temporary / "outside-panel-caller"
other_directory.mkdir()
external_call = subprocess.run(outside_args, cwd=other_directory, env=environment,
                               capture_output=True, text=True, check=True)
outside = json.loads(external_call.stdout)
assert outside["plan_binding"]["plan_id"] == "plan_elsewhere" and outside["main_attempt_id"] is None, outside
# A result may close only workers from its plan generation and created before
# that revision. A later worker remains pending even while older work settles.
import review_settlement
decision = dict(outside, main_attempt_id=main_attempts["codex"])
result_row = {"ts": "2026-10-16T00:00:00Z"}
old_worker = {"attempt_id": "att_before-result", "task_id": "elsewhere-task",
              "parent_attempt_id": main_attempts["codex"], "state": "review", "terminal": False,
              "created_at": "2026-10-15T00:00:00Z", "refs": {"panel_role": "worker"}}
new_worker = dict(old_worker, attempt_id="att_after-result", created_at="2026-10-17T00:00:00Z")
event_command = ["bash", str(panel.ENTRY), "agent-events", "--repo", str(project)]
lifecycle = __import__("runpy").run_path(str(root / "scripts/lib/agent-events.py"))
late_review_ident = call(event_command + ["start", "--provider", "local", "--tool", "peer-delegate",
    "--task-id", "elsewhere-task", "--parent-attempt-id", main_attempts["codex"],
    "--ref", "panel_role=worker", "--then", "starting", "--then", "working", "--then",
    "verifying", "--then", "review"]).strip()
late_review_worker = dict(old_worker, attempt_id=late_review_ident)
with patch.object(review_settlement, "_stopped", return_value=True), \
        patch.object(review_settlement, "_apply") as settle_apply:
    report = review_settlement._settle(project, [old_worker, new_worker, late_review_worker], [],
        {main_attempts["codex"]}, {"elsewhere-task": (decision, result_row)}, apply=True)
assert report["applied"] == [old_worker["attempt_id"]], report
assert {(row["attempt"], row["pending_reason"]) for row in report["pending"]} == {
    (new_worker["attempt_id"], "newer_than_decision"),
    (late_review_worker["attempt_id"], "newer_than_decision")}, report
assert settle_apply.call_count == 1
# If lifecycle evidence cannot be read, an unknown review-entry time must not
# fall back to creation time and settle work that may have entered review late.
import runpy
run_path = runpy.run_path
def unreadable_events(path, *args, **kwargs):
    loaded = run_path(path, *args, **kwargs)
    if str(path).endswith("scripts/lib/agent-events.py"):
        def fail_read(_path):
            raise OSError("fixture lifecycle ledger unavailable")
        loaded["read_rows"] = fail_read
    return loaded
with patch.object(runpy, "run_path", side_effect=unreadable_events), \
        patch.object(review_settlement, "_stopped", return_value=True):
    unreadable_lifecycle = review_settlement._settle(project, [late_review_worker], [],
        {main_attempts["codex"]}, {"elsewhere-task": (decision, result_row)}, apply=False)
assert unreadable_lifecycle["pending"][0]["attempt"] == late_review_ident, unreadable_lifecycle
assert unreadable_lifecycle["pending"][0]["pending_reason"] == "lifecycle_unreadable", unreadable_lifecycle
# Exercise persistence and lifecycle writes: settlement must not mint another
# result revision or notification when an identical finalize is retried.
exit_artifact = project / ".oms/artifacts/settlement-exit.md"
exit_artifact.write_text("## Output\nCompleted fixture work.\n## Exit\n0\n")
def settlement_worker(task, parent=None, review=True, legacy=False, exit_proof=True):
    command = event_command + ["start", "--provider", "local", "--tool", "peer-delegate", "--task-id", task,
        "--then", "starting", "--then", "working"]
    if parent:
        command += ["--parent-attempt-id", parent]
    if not legacy:
        command += ["--ref", "panel_role=worker"]
    ident = call(command + (["--then", "verifying", "--then", "review"] if review else [])).strip()
    if review and exit_proof:
        call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" delegate local 0 "$3"', "settlement-fixture",
              str(root / "scripts/lib/peer-common.sh"), str(project), str(exit_artifact)],
             dict(environment, OMS_TASK_ID=task, OMS_ATTEMPT_ID=ident))
    return ident
for new_owner, harness in (("pi", "pi"), ("antigravity", "agy"), ("codex", "pi")):
    owner_task = "new-owner-" + new_owner
    if new_owner == "codex":  # maximum panel id length keeps the bounded resolve reason valid
        owner_task = owner_task + "x" * (160 - len(owner_task))
        assert len(owner_task) == 160
    owner_room = room.create(project, owner_task, "Main owner result")
    owner_main = call(event_command + ["start", "--provider", new_owner, "--tool", "panel-main",
        "--ref", "panel_role=main", "--ref", "panel_owner=" + new_owner,
        "--ref", "panel_harness=" + harness, "--ref", "panel_room_id=" + owner_room,
        "--then", "starting", "--then", "working"]).strip()
    room.join(project, owner_room, owner_main, new_owner)
    owner_worker = settlement_worker(owner_task, owner_main)
    owner_environment = dict(environment, OMS_AGENT=new_owner, OMS_PANEL_MAIN_ATTEMPT=owner_main,
                             OMS_ROOM_PARTICIPANT=owner_main, OMS_ROOM_ID=owner_room)
    owner_plan_task = dict(id=owner_task, claimed_by_participant=owner_main, provider=new_owner,
                           lease_id="lease_" + "1" * 32)
    with patch.dict(os.environ, owner_environment, clear=True), \
            patch.object(saved, "_read", return_value=json.dumps({"plan_id": "plan_" + "2" * 32,
                "tasks": {owner_task: owner_plan_task}})):
        assert saved._owned_task(project, project, new_owner, owner_task)["provider"] == new_owner
    owner_finalize = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--finalize",
        "--task-id", owner_task, "--owner", new_owner, "--summary-file", str(summary), "--outcome", "completed"]
    # Settlement keeps same-second review and decision timestamps pending.
    owner_review_at = json.loads(call(event_command + ["show", "--attempt", owner_worker, "--json"]))["updated_at"]
    owner_clock_deadline = time.monotonic() + int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90"))
    while time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()) <= owner_review_at:
        assert time.monotonic() < owner_clock_deadline, ("clock did not advance past review", owner_review_at)
        time.sleep(.01)
    def seed_failure(task, kind, code="1"):
        call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" "$3" local "$4" "$5"', "settlement-fixture",
              str(root / "scripts/lib/peer-common.sh"), str(project), kind, code, str(exit_artifact)],
             dict(environment, OMS_TASK_ID=task))
    def unresolved_events():
        listed = json.loads(call(["bash", str(root / "scripts/artifact-index.sh"), "--repo", str(project),
                                  "--json", "unresolved"]))
        return {(r.get("task_id"), r.get("kind")) for r in listed["rows"]}
    seed_failure(owner_task, "delegate")
    seed_failure(owner_task, "review-outcome", "3")
    other_task = owner_task[:-6] + "-other"
    seed_failure(other_task, "delegate")
    assert {(owner_task, "delegate"), (owner_task, "review-outcome")} <= unresolved_events()
    owner_result = json.loads(call(owner_finalize, owner_environment))
    assert owner_result["owner"] == new_owner, owner_result
    left = unresolved_events()
    assert not {t for t, _ in left} & {owner_task} and (other_task, "delegate") in left, left
    owner_state = json.loads(call(["bash", str(root / "scripts/state.sh"), "--repo", str(project), "--json"]))
    assert owner_state["artifacts"]["counts"]["unresolved"] == len(
        json.loads(call(["bash", str(root / "scripts/artifact-index.sh"), "--repo", str(project),
                         "--json", "unresolved"]))["rows"]), owner_state["artifacts"]["counts"]
    seed_failure(owner_task, "delegate")
    assert (owner_task, "delegate") in unresolved_events()
    assert json.loads(call(owner_finalize, owner_environment))["revision"] == owner_result["revision"]
    owner_rows = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                                 "--results", "--task-id", owner_task, "--json"], owner_environment))
    assert not owner_rows["rows"][0]["missing_result"], owner_rows
    assert json.loads((project / owner_rows["rows"][0]["artifact"]).read_text())["owner"] == new_owner
    settled_owner = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
        "--settle-review", "--owner", new_owner, "--apply", "--json"], owner_environment))
    assert not settled_owner["errors"], settled_owner
    owner_worker_state = json.loads(call(event_command + ["show", "--attempt", owner_worker, "--json"]))
    assert owner_worker_state["state"] == "done", (new_owner, owner_worker_state, owner_result["settlement"], settled_owner)
    call(event_command + ["transition", "--attempt", owner_main, "--state", "cancelled"])
    room.append(project, owner_room, {"kind": "leave", "participant": owner_main}, "Fixture main stopped")
summary_target = temporary / "summary-symlink-target.txt"
summary_target.write_text("Real summary.\n")
summary_link = temporary / "summary-symlink.txt"
summary_link.symlink_to(summary_target)
try:
    saved._safe_summary(summary_link, project)
except ValueError as error:
    assert "summary file unavailable or unsafe" in str(error), error
else:
    raise AssertionError("a symlinked summary file was accepted")
summary_invalid = temporary / "summary-invalid-utf8.txt"
summary_invalid.write_bytes(b"\xff\xfe")
try:
    saved._safe_summary(summary_invalid, project)
except ValueError as error:
    assert "process substitution" not in str(error), error
else:
    raise AssertionError("a non-UTF-8 summary file was accepted")
for invalid_owner in ("", "unknown", "agy", 0, None, []):
    try:
        saved.finalize(project, invalid_owner, "invalid-owner", str(summary), "completed")
    except ValueError as error:
        assert "owner" in str(error), error
    else:
        raise AssertionError("invalid result owner accepted")
    with patch.object(saved, "_indexed", return_value=json.dumps(dict(owner_result, owner=invalid_owner))):
        try:
            saved._load_result(project, {"task_id": owner_task})
        except ValueError:
            pass
        else:
            raise AssertionError("invalid stored result owner accepted")
stopped = settlement_worker("elsewhere-task", main_attempts["codex"])
running = settlement_worker("elsewhere-task", main_attempts["codex"], review=False)
time.sleep(1.1)  # The lifecycle and artifact ledgers record whole seconds.
with patch.dict(os.environ, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]), clear=True), \
        patch.object(saved.codex_app_notify, "deliver", return_value={"status": "sent", "persisted": True}) as notify:
    settled = saved.finalize(project, "codex", "elsewhere-task", str(summary), "completed",
                             "test -f value.txt", ["value.txt"], notify=True)
    assert settled["settlement"]["applied"] == [stopped], settled
    assert running in {r["attempt"] for r in settled["settlement"]["pending"]}, settled
    retry = saved.finalize(project, "codex", "elsewhere-task", str(summary), "completed",
                           "test -f value.txt", ["value.txt"], notify=True)
assert retry["revision"] == settled["revision"] and notify.call_count == 1, (settled, retry, notify.call_count)
assert json.loads(call(event_command + ["show", "--attempt", stopped, "--json"]))["state"] == "done"
assert json.loads(call(event_command + ["show", "--attempt", running, "--json"]))["state"] == "working"
assert json.loads(plan_path.read_text())["tasks"]["elsewhere-task"]["state"] == "review"
# Retrying an older finalize revision never settles against the newer decision.
superseded = review_settlement.after_finalize(project, dict(outside, main_attempt_id=main_attempts["codex"]))
assert superseded.get("skipped") == "superseded_revision", superseded
def finalize_plan_fixture(label, assigned):
    repo = temporary / label
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    env = dict(environment, OMS_AGENT="claude", OMS_ROOM_REPO=str(repo),
               OMS_ROOM_ID="bound-finalize-room", OMS_ROOM_PARTICIPANT="bound-finalize-main")
    with patch.dict(os.environ, env, clear=True):
        room.create(repo, env["OMS_ROOM_ID"], "Bound finalize")
        room.join(repo, env["OMS_ROOM_ID"], env["OMS_ROOM_PARTICIPANT"], "claude")
    env["OMS_PANEL_MAIN_ATTEMPT"] = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(repo),
        "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
        "--ref", "panel_room_id=" + env["OMS_ROOM_ID"],
        "--ref", "panel_room_participant=" + env["OMS_ROOM_PARTICIPANT"],
        "--ref", "panel_harness=claude", "--then", "starting", "--then", "working"], env).strip()
    command = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(repo)]
    call(command + ["init", "--goal", "Bound finalize ownership"], env)
    call(command + ["add", "--id", "owned-task", "--title", "Assigned result", "--allowed", "value.txt",
                    "--verify", "true", "--assignment", json.dumps({"provider": assigned, "workload": "routine"})], env)
    call(command + ["claim", "--id", "owned-task", "--provider", "claude"], env)
    task = json.loads(call(command + ["evidence-snapshot", "--id", "owned-task"], env))
    assert task["provider"] == assigned and task["claimed_by_participant"] == env["OMS_ROOM_PARTICIPANT"], task
    return repo, env, task

# Ownership checks fail closed without changing accepted-result diagnostics.
def exercise_finalize_ownership_snapshot():
    main = dict(attempt_id="own-main", tool="panel-main", provider="codex", state="working",
                terminal=False, refs={"panel_role": "main", "panel_room_participant": "own"})
    other = dict(main, attempt_id="other-main", refs={"panel_role": "main", "panel_room_participant": "other"})
    prior = dict(main, attempt_id="prior-main", state="cancelled", terminal=True)
    worker = dict(attempt_id="worker", task_id="owned-task", parent_attempt_id="own-main", refs={})
    nested = dict(worker, attempt_id="nested", parent_attempt_id="worker")
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": "own"}):
        for rows in ([], [main], [main, worker], [prior, main, dict(worker, parent_attempt_id="prior-main")],
                     [main, worker, nested],
                     [{"attempt_id": "own-main", "tool": "panel-main", "provider": "codex"}, worker]):
            saved._finalize_owns_task(project, "codex", "owned-task", "own-main", rows, None)
        saved._finalize_owns_task(project, "codex", "owned-task", None, [main, worker], None)
        invalid_rows = [[main, other, dict(worker, parent_attempt_id=parent)]
                        for parent in (None, "", 0, "missing", "worker", "other-main")]
        invalid_rows += [[main, worker, dict(worker, attempt_id="foreign", parent_attempt_id="other-main"), other],
                         [dict(main, refs={"panel_role": "main", "panel_room_participant": "other"}), prior,
                          dict(worker, parent_attempt_id="prior-main")]]
        for rows in invalid_rows:
            try:
                saved._finalize_owns_task(project, "codex", "owned-task", "own-main", rows, None)
            except ValueError as error:
                assert "calling main" in str(error), error
            else:
                raise AssertionError("invalid main lineage accepted")
    bound_repo, bound_env, bound_task = finalize_plan_fixture("assigned-finalize", "codex")
    with patch.dict(os.environ, bound_env, clear=True):
        for outcome in ("completed", "failed", "rejected"):
            result = saved.finalize(bound_repo, "claude", "owned-task", str(summary), outcome)
            assert result["outcome"] == outcome and result["owner"] == "claude", result
        # Accept/finish keeps its stricter provider contract.
        try:
            saved._owned_task(bound_repo, bound_repo, "claude", "owned-task")
        except ValueError as error:
            assert "owner/lease" in str(error), error
        else:
            raise AssertionError("accept/finish provider mismatch accepted")
        for state in ("claimed", "done"):
            plan = {"tasks": {"owned-task": dict(bound_task, state=state)}}
            with patch.object(saved, "_read", return_value=json.dumps(plan)):
                saved._finalize_owns_task(bound_repo, "claude", "owned-task", bound_env["OMS_PANEL_MAIN_ATTEMPT"], [], {})
        for holder in ("other", 0, [], {}):
            plan = {"tasks": {"owned-task": dict(bound_task, claimed_by_participant=holder)}}
            with patch.object(saved, "_read", return_value=json.dumps(plan)):
                try:
                    saved._finalize_owns_task(bound_repo, "claude", "owned-task", bound_env["OMS_PANEL_MAIN_ATTEMPT"], [], {})
                except ValueError as error:
                    assert "not owned by the calling main" in str(error), error
                else:
                    raise AssertionError("foreign or malformed plan claimant accepted")
        for task in ({"id": "owned-task"}, {"id": "owned-task", "claimed_by_participant": ""}):
            with patch.object(saved, "_read", return_value=json.dumps({"tasks": {"owned-task": task}})):
                saved._finalize_owns_task(bound_repo, "claude", "owned-task", bound_env["OMS_PANEL_MAIN_ATTEMPT"], [], {})
        for body in ("malformed", "null", "0", "{}", '{"tasks": []}',
                     '{"tasks":{"owned-task":{"id":"wrong"}}}',
                     '{"tasks":{"owned-task":{"id":"owned-task","id":"owned-task"}}}'):
            with patch.object(saved, "_read", return_value=body):
                try:
                    saved._finalize_owns_task(bound_repo, "claude", "owned-task", bound_env["OMS_PANEL_MAIN_ATTEMPT"], [], {})
                except ValueError as error:
                    assert "not owned by the calling main" in str(error), error
                else:
                    raise AssertionError("malformed plan ownership accepted: " + body)
        for identities in ({"OMS_ROOM_PARTICIPANT": "other"}, {"OMS_ROOM_ID": ""}):
            with patch.dict(os.environ, identities):
                try:
                    saved.finalize(bound_repo, "claude", "owned-task", str(summary), "completed")
                except ValueError as error:
                    assert "not owned by the calling main" in str(error), error
                else:
                    raise AssertionError("foreign or missing room caller accepted")
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(bound_repo), "transition",
          "--attempt", bound_env["OMS_PANEL_MAIN_ATTEMPT"], "--state", "cancelled"], bound_env)
exercise_finalize_ownership_snapshot()
# Finalize authorization follows participant lineage, including a resumed main.
def exercise_finalize_task_ownership():
    foreign_task = "finalize-read-task"
    foreign_worker = settlement_worker(foreign_task, main_attempts["codex"], review=False)
    bad_main = call(event_command + ["start", "--provider", "codex", "--tool", "panel-main",
        "--ref", "panel_role=main", "--ref", "panel_room_participant=finalize-other",
        "--then", "starting", "--then", "working"]).strip()
    bad_env = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=bad_main,
                   OMS_ROOM_PARTICIPANT="finalize-other")
    with patch.dict(os.environ, bad_env, clear=True):
        for outcome in ("completed", "failed", "rejected", "accepted"):
            try:
                saved.finalize(project, "codex", foreign_task, str(summary), outcome, "true", ["value.txt"])
            except ValueError as error:
                assert "calling main" in str(error), error
            else:
                raise AssertionError("different main finalized a task it does not own: " + outcome)
    prior_main = call(event_command + ["start", "--provider", "codex", "--tool", "panel-main",
        "--ref", "panel_role=main", "--ref", "panel_room_participant=finalize-resumed",
        "--then", "starting", "--then", "working"]).strip()
    task = "finalize-resumed-task"
    resumed_worker = settlement_worker(task, prior_main, review=False)
    call(event_command + ["transition", "--attempt", prior_main, "--state", "cancelled"])
    resumed_main = call(event_command + ["start", "--provider", "codex", "--tool", "panel-main",
        "--ref", "panel_role=main", "--ref", "panel_room_participant=" + "finalize-resumed",
        "--then", "starting", "--then", "working"]).strip()
    resumed_env = dict(environment, OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=resumed_main,
                       OMS_ROOM_PARTICIPANT="finalize-resumed")
    with patch.dict(os.environ, resumed_env, clear=True):
        result = saved.finalize(project, "codex", task, str(summary), "completed")
        assert result["outcome"] == "completed" and result["main_attempt_id"] == resumed_main, result
    swap_repo, swap_env, swap_task = finalize_plan_fixture("swapped-finalize", "claude")
    swap_events = ["bash", str(panel.ENTRY), "agent-events", "--repo", str(swap_repo)]
    prior = swap_env["OMS_PANEL_MAIN_ATTEMPT"]
    prior_worker = call(swap_events + ["start", "--provider", "local", "--tool", "peer-delegate",
        "--task-id", "owned-task", "--parent-attempt-id", prior, "--ref", "panel_role=worker",
        "--then", "starting", "--then", "working"], swap_env).strip()
    call(swap_events + ["transition", "--attempt", prior, "--state", "blocked", "--then", "abandoned",
                       "--actor", "panel-main", "--reason-code", "main_swapped"], swap_env)
    with patch.dict(os.environ, swap_env, clear=True):
        room.swap_main(swap_repo, swap_env["OMS_ROOM_ID"], swap_env["OMS_ROOM_PARTICIPANT"],
                       "codex", "gpt-6.1-terra", prior, "codex")
    swapped = call(swap_events + ["start", "--provider", "codex", "--tool", "panel-main",
        "--ref", "panel_role=main", "--ref", "panel_room_id=" + swap_env["OMS_ROOM_ID"],
        "--ref", "panel_room_participant=" + swap_env["OMS_ROOM_PARTICIPANT"],
        "--ref", "panel_harness=codex", "--then", "starting", "--then", "working"], swap_env).strip()
    swap_env.update(OMS_AGENT="codex", OMS_PANEL_MAIN_ATTEMPT=swapped)
    with patch.dict(os.environ, swap_env, clear=True):
        assert prior in panel.lineage_ids_from_rows(saved._records(swap_repo, full=True)[0],
            "codex", swap_env["OMS_ROOM_PARTICIPANT"], swapped)
        result = saved.finalize(swap_repo, "codex", "owned-task", str(summary), "completed")
        assert result["owner"] == "codex" and result["main_attempt_id"] == swapped, result
        stored = json.loads((swap_repo / ".oms/plan/tasks.json").read_text())["tasks"]["owned-task"]
        assert stored["provider"] == "claude" and stored["lease_id"] == swap_task["lease_id"], stored
    for ident in (swapped, prior_worker):
        call(swap_events + ["transition", "--attempt", ident, "--state", "cancelled"], swap_env)
    parentless_task = "finalize-parentless-task"
    parentless = settlement_worker(parentless_task, review=False)
    with patch.dict(os.environ, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]), clear=True):
        try:
            saved.finalize(project, "codex", parentless_task, str(summary), "completed")
        except ValueError as error:
            assert parentless in str(error) and "does not descend" in str(error) and "\n" not in str(error), error
        else:
            raise AssertionError("parentless attempt was finalized")
    call(event_command + ["transition", "--attempt", parentless, "--state", "cancelled"])
    for ident in (bad_main, resumed_main, foreign_worker, resumed_worker):
        call(event_command + ["transition", "--attempt", ident, "--state", "cancelled"])
exercise_finalize_task_ownership()
# A proven-dead main uses its participant history, rather than the live-only
# continuation helper. Keep a saved result whose first settlement was deferred.
import panel_release
gone = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"], stdin=subprocess.PIPE)
gone_refs = dict(panel_release.process_refs(gone.pid, "wrapper"), **panel_release.process_refs(gone.pid, "native"))
gone.communicate(b"", timeout=10)
dead_command = event_command + ["start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_participant=settlement-dead-main", "--ref", "panel_host=inline"]
for name, value in gone_refs.items():
    dead_command += ["--ref", name + "=" + value]
dead_main = call(dead_command + ["--then", "starting", "--then", "working"]).strip()
dead_task = "settlement-dead-task"
external_plan["tasks"][dead_task] = dict(external_plan["tasks"]["elsewhere-task"], id=dead_task)
write_external_plan()
dead_worker = settlement_worker(dead_task, dead_main)
time.sleep(1.1)
with patch.dict(os.environ, dict(environment, OMS_PANEL_MAIN_ATTEMPT=dead_main), clear=True), \
        patch.object(review_settlement, "_apply", side_effect=ValueError("fixture deferred settlement")):
    settlement_output = io.StringIO()
    with redirect_stderr(settlement_output):
        deferred = saved.finalize(project, "codex", dead_task, str(summary), "completed")
assert "--settle-review --owner codex" in settlement_output.getvalue() and dead_task in settlement_output.getvalue()
assert deferred["settlement"]["errors"] and not deferred["settlement"]["applied"], deferred
call(event_command + ["transition", "--attempt", dead_main, "--state", "cancelled"])
dead_successor = call(dead_command + ["--then", "starting", "--then", "working"]).strip()
call(event_command + ["transition", "--attempt", dead_successor, "--state", "cancelled"])
recover = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--settle-review",
           "--owner-attempt", dead_successor, "--apply", "--json"]
recovered = json.loads(call(recover))
assert recovered["applied"] == [dead_worker] and not recovered["errors"], recovered
assert json.loads(call(event_command + ["show", "--attempt", dead_worker, "--json"]))["state"] == "done"
refused_live = subprocess.run(recover[:recover.index("--owner-attempt")] + [
    "--owner-attempt", main_attempts["codex"], "--apply"], env=environment, capture_output=True)
assert refused_live.returncode, "recovery selected a live owner"
# Legacy recovery needs opt-in and the age of the last event, never creation age.
legacy_main = call(event_command + ["start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--then", "starting", "--then", "working", "--then", "cancelled"]).strip()
legacy_worker = settlement_worker("settlement-legacy-task", legacy_main)
legacy_args = recover[:recover.index("--owner-attempt")] + ["--owner-attempt", legacy_main,
    "--attempt", legacy_worker, "--reason", "legacy fixture", "--json"]
for extra, message in (([], "recorded host ownership unavailable"),
                       (["--legacy"], "last lifecycle event")):
    refused = subprocess.run(legacy_args + extra, env=environment, capture_output=True, text=True)
    assert refused.returncode and message in refused.stderr, refused
legacy_row = json.loads(call(event_command + ["show", "--attempt", legacy_main, "--json"]))
aged_legacy = dict(legacy_row, created_at="2000-01-01T00:00:00Z", updated_at="2000-01-02T00:00:00Z")
assert review_settlement._participant_parents([aged_legacy], aged_legacy, True, 24) == {legacy_main}
for changed in ({"updated_at": legacy_row["updated_at"]}, {"updated_at": "invalid"},
                {"updated_at": "2999-01-01T00:00:00Z"}, {"updated_at": None},
                {"terminal": False, "state": "working"},
                *({"refs": dict(aged_legacy["refs"], **{key: ""})} for key in
                  ("panel_host", "panel_wrapper_pid", "panel_native_identity", "panel_pane_id", "panel_launcher_pid"))):
    guarded = dict(aged_legacy, **changed)
    try:
        review_settlement._participant_parents([guarded], guarded, True, 24)
    except ValueError:
        pass
    else:
        raise AssertionError("legacy opt-in bypassed age, state or recorded ownership: " + repr(changed))
with patch.object(review_settlement, "_age", return_value=86400):
    try:
        review_settlement._participant_parents([aged_legacy], aged_legacy, True, 24)
    except ValueError:
        pass
    else:
        raise AssertionError("legacy threshold boundary accepted")
time.sleep(1.1)
legacy_args += ["--legacy", "--legacy-older-than", "0.000001"]
legacy_preview = json.loads(call(legacy_args))
assert not legacy_preview["applied"] and not legacy_preview["executes"], legacy_preview
assert legacy_preview["candidates"][0]["selection_reason"] == "legacy_terminal_main", legacy_preview
assert legacy_preview["candidates"][0]["legacy_older_than_hours"] == 0.000001, legacy_preview
assert json.loads(call(event_command + ["show", "--attempt", legacy_worker, "--json"]))["state"] == "review"
assert json.loads(call(legacy_args + ["--apply"]))["applied"] == [legacy_worker]
assert not json.loads(call(legacy_args + ["--apply"]))["applied"], "legacy retry reapplied"
# Standalone legacy calls have no panel_role ref. Only aged reviews with a
# matching indexed Exit proof may be cancelled, regardless of main environment.
standalone_worker = settlement_worker("settlement-standalone-task", legacy=True)
orphan_worker = settlement_worker("settlement-orphan-task", "att_missing_parent")
owned_worker = settlement_worker("settlement-owned-task", main_attempts["codex"])
unproven_worker = settlement_worker("settlement-no-exit-task", legacy=True, exit_proof=False)
standalone_running = settlement_worker("settlement-running-task", review=False, legacy=True)
standalone_args = recover[:recover.index("--owner-attempt")] + ["--standalone", "--older-than", "1", "--json"]
young = json.loads(call(standalone_args))
assert standalone_worker in {r["attempt"] for r in young["pending"] if r["pending_reason"] == "review_too_recent"}, young
assert orphan_worker in {r["attempt"] for r in young["pending"] if r["pending_reason"] == "review_too_recent"}, young
assert owned_worker not in {r["attempt"] for r in young["pending"] + young["candidates"]}, young
with patch.object(review_settlement, "_age", return_value=None), \
        patch.dict(os.environ, environment, clear=True):
    unknown_age = review_settlement.settle_review(project, standalone=True, older_than=1)
assert standalone_worker in {r["attempt"] for r in unknown_age["pending"] if r["pending_reason"] == "review_age_unknown"}
time.sleep(1.1)
standalone_args[standalone_args.index("--older-than") + 1] = "0.00000001"
preview = json.loads(call(standalone_args))
assert standalone_worker in {r["attempt"] for r in preview["candidates"]}, preview
assert orphan_worker in {r["attempt"] for r in preview["candidates"] if r["selection_reason"] == "aged_standalone_review"}, preview
assert owned_worker not in {r["attempt"] for r in preview["candidates"] + preview["pending"]}, preview
assert not preview["executes"] and not preview["applied"], preview
assert unproven_worker in {r["attempt"] for r in preview["pending"] if r["pending_reason"] == "exit_unknown"}, preview
assert not {standalone_running, running} & {r["attempt"] for r in preview["candidates"]}, preview
standalone_rows, standalone_artifacts = saved._records(project, full=True)
standalone_attempt = next(a for a in standalone_rows if a["attempt_id"] == standalone_worker)
orphan_attempt = next(a for a in standalone_rows if a["attempt_id"] == orphan_worker)
with patch.object(review_settlement, "_age", return_value=86400):
    boundary = review_settlement._settle(project, [orphan_attempt], standalone_artifacts, set(), {},
                                         standalone_older_than=1)
assert boundary["pending"][0]["pending_reason"] == "review_too_recent", boundary
indexed_exit = next(r for r in standalone_artifacts if r.get("attempt_id") == standalone_worker)
for artifact in (dict(indexed_exit, exit=1), dict(indexed_exit, artifact_sha256="0" * 64)):
    unproven = review_settlement._settle(project, [standalone_attempt], [artifact], set(), {},
                                        standalone_older_than=0.00000001)
    assert not unproven["candidates"] and not unproven["errors"], unproven
    assert unproven["pending"][0]["pending_reason"] == "exit_unknown", unproven
bound_standalone = dict(standalone_attempt, refs={"panel_room_participant": "unknown", "panel_room_id": "unknown"})
with patch.object(review_settlement, "_stopped", side_effect=AssertionError("room proof used for standalone")):
    unproven = review_settlement._settle(project, [bound_standalone], [indexed_exit], set(), {},
                                        standalone_older_than=0.00000001)
assert unproven["pending"][0]["pending_reason"] == "exit_unknown", unproven
settlement = json.loads(call(standalone_args + ["--apply"]))
assert standalone_worker in settlement["applied"] and unproven_worker not in settlement["applied"], settlement
assert orphan_worker in settlement["applied"] and owned_worker not in settlement["applied"], settlement
assert json.loads(call(event_command + ["show", "--attempt", orphan_worker, "--json"]))["state"] == "cancelled"
assert json.loads(call(event_command + ["show", "--attempt", owned_worker, "--json"]))["state"] == "review"
standalone_final = json.loads(call(event_command + ["show", "--attempt", standalone_worker, "--json"]))
assert standalone_final["state"] == "cancelled" and standalone_final["reason_code"] == "owner_superseded", standalone_final
assert standalone_final["refs"]["owner_superseded_reason"] == "aged standalone review", standalone_final
assert standalone_worker not in json.loads(call(standalone_args + ["--apply"]))["applied"]
def check_room_bound_standalone(project, standalone_attempt, indexed_exit):
    import dispatch_supervisor as dispatch_store
    participant = "call-settlement-fixture"
    room_id = "settlement-room-bound"
    bound = dict(standalone_attempt, refs={"panel_room_participant": participant, "panel_room_id": room_id})
    args = (project, [bound], [indexed_exit], set(), {})
    kwargs = {"standalone_older_than": 1}
    with patch.object(review_settlement, "_age", return_value=86401):
        with patch.object(dispatch_store, "read", side_effect=FileNotFoundError), \
                patch.object(panel, "_dispatch_call_finished") as finished:
            result = review_settlement._settle(*args, **kwargs)
            assert [row["attempt"] for row in result["candidates"]] == [bound["attempt_id"]], result
            finished.assert_not_called()
        anchor = {"call": {"parent": "launch-parent"}}
        for is_finished in (True, False):
            with patch.object(dispatch_store, "read", return_value=anchor) as read, \
                    patch.object(panel, "_dispatch_call_finished", return_value=is_finished) as finished:
                result = review_settlement._settle(*args, **kwargs)
                read.assert_called_once_with(project, participant + "-launch.json")
                finished.assert_called_once_with(project, room_id,
                    {"participant": participant, "parent": "launch-parent"})
                if is_finished:
                    assert result["candidates"] and not result["pending"], result
                else:
                    assert not result["candidates"] and result["pending"][0]["pending_reason"] == "exit_unknown", result
        for error in (PermissionError("unreadable"), ValueError("malformed")):
            with patch.object(dispatch_store, "read", side_effect=error):
                result = review_settlement._settle(*args, **kwargs)
                assert not result["candidates"] and result["pending"][0]["pending_reason"] == "exit_unknown", result
        for anchor in (None, {}, {"call": {}}):
            with patch.object(dispatch_store, "read", return_value=anchor):
                result = review_settlement._settle(*args, **kwargs)
                assert not result["candidates"] and result["pending"][0]["pending_reason"] == "exit_unknown", result
        with patch.object(dispatch_store, "read", side_effect=AssertionError("unproven exit read launch")):
            result = review_settlement._settle(project, [bound], [dict(indexed_exit, exit=99)], set(), {}, **kwargs)
            assert not result["candidates"] and result["pending"][0]["pending_reason"] == "exit_unknown", result
        for refs in ({"panel_room_participant": participant}, {"panel_room_id": room_id}):
            partial = dict(bound, refs=refs)
            result = review_settlement._settle(project, [partial], [indexed_exit], set(), {}, **kwargs)
            assert not result["candidates"] and result["pending"][0]["pending_reason"] == "exit_unknown", result
    for age in (None, 0, 86400):
        with patch.object(review_settlement, "_age", return_value=age), \
                patch.object(dispatch_store, "read", side_effect=AssertionError("too recent read launch")):
            result = review_settlement._settle(*args, **kwargs)
            expected = "review_age_unknown" if age is None else "review_too_recent"
            assert not result["candidates"] and result["pending"][0]["pending_reason"] == expected, result

check_room_bound_standalone(project, standalone_attempt, indexed_exit)

# The explicit seven-day policy covers undecided legacy work and missing exits.
# Append through the lifecycle API with a controlled event clock, never rewrite
# the fixture ledger; a later review entry must replace the earlier review age.
def settlement_transition(ident, state, stamp="2000-01-01T00:00:00Z"):
    with patch.dict(lifecycle["new_event"].__globals__, {"utc_now": lambda: stamp}):
        event = lifecycle["new_event"](ident, 0, "attempt.state_changed", from_state=None,
            to_state=state, actor={"kind": "owner", "name": "settlement-fixture"})
    lifecycle["append_lifecycle"](project, event)

undecided = settlement_worker("settlement-undecided-task", legacy_main, review=False, exit_proof=False)
settlement_transition(undecided, "verifying")
settlement_transition(undecided, "review")
legacy_aged_args = recover[:recover.index("--owner-attempt")] + ["--owner-attempt", legacy_main,
    "--legacy", "--legacy-older-than", "0.000001", "--json"]
assert not json.loads(call(legacy_aged_args))["candidates"], "undecided work selected without opt-in"
legacy_aged_args += ["--aged-days", "7"]
recent_review = settlement_worker("settlement-recent-task", legacy_main, exit_proof=False)
reentered_review = settlement_worker("settlement-reentered-task", legacy_main, review=False, exit_proof=False)
for state in ("verifying", "review", "working", "verifying"):
    settlement_transition(reentered_review, state)
call(event_command + ["transition", "--attempt", reentered_review, "--state", "review"])
aged_preview = json.loads(call(legacy_aged_args))
assert [r["attempt"] for r in aged_preview["candidates"]] == [undecided], aged_preview
assert not aged_preview["executes"] and not aged_preview["applied"], aged_preview
assert {recent_review, reentered_review} <= {r["attempt"] for r in aged_preview["pending"]
    if r["pending_reason"] == "review_too_recent"}, aged_preview
assert aged_preview["candidates"][0]["reason"] == "aged review without owner decision (7 days)"
own_aged = settlement_worker("settlement-own-aged-task", main_attempts["codex"], exit_proof=False)
settlement_transition(own_aged, "working")
settlement_transition(own_aged, "verifying")
settlement_transition(own_aged, "review")
own_args = recover[:recover.index("--owner-attempt")] + ["--aged-days", "7", "--json"]
own_preview = json.loads(call(own_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])))
assert own_aged in {r["attempt"] for r in own_preview["candidates"]}, own_preview
assert not own_preview["executes"] and not own_preview["applied"], own_preview
blocked_worker = settlement_worker("settlement-blocked-task", main_attempts["codex"], review=False)
settlement_transition(blocked_worker, "blocked")
blocked_rows, _ = saved._records(project, full=True)
blocked_preview = review_settlement._settle(project, blocked_rows, [],
    {main_attempts["codex"]}, {}, aged_days=7)
assert blocked_worker in {r["attempt"] for r in blocked_preview["candidates"]}, blocked_preview
assert next(r for r in blocked_preview["candidates"] if r["attempt"] == blocked_worker)["reason"] == \
    "aged blocked without owner decision (7 days)", blocked_preview
explicit_blocked = review_settlement._settle(project, blocked_rows, [],
    {main_attempts["codex"]}, {}, explicit=[blocked_worker], reason="owner choice")
assert explicit_blocked["pending"][0]["pending_reason"] == "aged_days_required", explicit_blocked
with patch.dict(os.environ, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]), clear=True):
    blocked_applied = review_settlement.settle_review(project, aged_days=7, apply=True)
assert blocked_worker in blocked_applied["applied"], blocked_applied
blocked_final = json.loads(call(event_command + ["show", "--attempt", blocked_worker, "--json"]))
assert blocked_final["state"] == "cancelled" and blocked_final["reason_code"] == "owner_superseded", blocked_final
assert blocked_final["refs"]["owner_superseded_reason"] == "aged blocked without owner decision (7 days)", blocked_final
assert json.loads(call(event_command + ["show", "--attempt", undecided, "--json"]))["state"] == "review"
for state in ("working", "verifying", "review"):
    settlement_transition(unproven_worker, state)
aged_standalone_args = recover[:recover.index("--owner-attempt")] + ["--standalone", "--aged-days", "7", "--json"]
aged_preview = json.loads(call(aged_standalone_args))
assert [r["attempt"] for r in aged_preview["candidates"]] == [unproven_worker], aged_preview
assert not aged_preview["executes"] and not aged_preview["applied"], aged_preview
with patch.dict(os.environ, environment, clear=True):
    for age, pending_reason in ((None, "review_age_unknown"), (7 * 86400, "review_too_recent")):
        with patch.object(review_settlement, "_age", return_value=age):
            guarded = review_settlement.settle_review(project, standalone=True, aged_days=7)
        assert not guarded["candidates"] and unproven_worker in {r["attempt"] for r in guarded["pending"]
            if r["pending_reason"] == pending_reason}, guarded
rows, artifacts = saved._records(project, full=True)
aged_attempt = next(a for a in rows if a["attempt_id"] == undecided)
for state in ("starting", "working", "verifying", "cancelled"):
    guarded = review_settlement._settle(project, [dict(aged_attempt, state=state)], [],
        {legacy_main}, {}, aged_days=7)
    assert not guarded["candidates"]
    if state == "cancelled":
        assert guarded["terminal"] == 1 and not guarded["pending"], guarded
    else:
        assert guarded["pending"][0]["pending_reason"] == "not_in_review", guarded
with patch.object(review_settlement, "_stopped", side_effect=AssertionError("aged policy used exit proof")):
    guarded = review_settlement._settle(project, [dict(bound_standalone, attempt_id=unproven_worker)], [],
        set(), {}, aged_days=7, standalone_mode=True)
assert not guarded["candidates"] and guarded["pending"][0]["pending_reason"] == "exit_unknown", guarded
refused_live = subprocess.run(recover[:recover.index("--owner-attempt")] + ["--owner-attempt",
    main_attempts["codex"], "--legacy", "--aged-days", "7"], env=environment, capture_output=True)
assert refused_live.returncode, "aged policy selected a live main's lineage"
# A state change after selection must still lose the expected-review-state CAS.
call(event_command + ["transition", "--attempt", undecided, "--state", "working"])
try:
    review_settlement._apply(project, aged_attempt, "cancelled", "owner_superseded",
                             aged_attempt["task_id"], "aged-cas-fixture")
except ValueError as error:
    assert "expected lifecycle state review, found working" in str(error), error
else:
    raise AssertionError("aged settlement bypassed expected-state CAS")
settlement_transition(undecided, "verifying")
settlement_transition(undecided, "review")
apply_selected = review_settlement._apply
def interleave_review(*args):
    call(event_command + ["transition", "--attempt", undecided, "--state", "working",
                          "--then", "verifying", "--then", "review"])
    return apply_selected(*args)

with patch.dict(os.environ, environment, clear=True), \
        patch.object(review_settlement, "_apply", side_effect=interleave_review):
    raced = review_settlement.settle_review(project, owner_attempt=legacy_main, legacy=True,
                                           legacy_older_than=0.000001, aged_days=7, apply=True)
assert not raced["applied"] and len(raced["errors"]) == 1, raced
assert "expected lifecycle sequence" in raced["errors"][0]["error"], raced
fresh_review = json.loads(call(event_command + ["show", "--attempt", undecided, "--json"]))
assert fresh_review["state"] == "review", fresh_review
assert not json.loads(call(legacy_aged_args))["candidates"], "re-entered review retained its old age"
for state in ("working", "verifying", "review"):
    settlement_transition(undecided, state)
assert json.loads(call(legacy_aged_args + ["--apply"]))["applied"] == [undecided]
with patch.dict(os.environ, environment, clear=True), patch.object(saved, "_lock", wraps=saved._lock) as result_lock:
    applied_aged = review_settlement.settle_review(project, standalone=True, aged_days=7, apply=True)
assert applied_aged["applied"] == [unproven_worker] and result_lock.call_count == 1, applied_aged
for ident in (undecided, unproven_worker):
    final = json.loads(call(event_command + ["show", "--attempt", ident, "--json"]))
    assert final["state"] == "cancelled" and final["reason_code"] == "owner_superseded", final
    assert final["refs"]["owner_superseded_reason"] == "aged review without owner decision (7 days)", final
assert not json.loads(call(legacy_aged_args + ["--apply"]))["applied"]
assert not json.loads(call(aged_standalone_args + ["--apply"]))["applied"]
def exercise_landed_standalone_review():
    """A standalone review whose exact patch bytes landed settles as the owner's decision."""
    import hashlib
    patch_ref = ".oms/artifacts/delegate/settlement-landed.patch"
    (project / patch_ref).parent.mkdir(parents=True, exist_ok=True)
    (project / patch_ref).write_bytes(b"settlement landed patch bytes\n")
    sha = hashlib.sha256((project / patch_ref).read_bytes()).hexdigest()
    other_sha = "b" * 64
    stamp = "2999-01-01T00:00:00Z"
    def state_of(ident):
        return json.loads(call(event_command + ["show", "--attempt", ident, "--json"]))["state"]
    def fixture(task, plan_id=None):
        ident = settlement_worker(task, legacy=True)
        attempts, artifacts = saved._records(project, full=True)
        attempt = next(a for a in attempts if a["attempt_id"] == ident)
        proof = next(r for r in artifacts if r.get("attempt_id") == ident and r.get("kind") == "delegate")
        # The fixture's delegate row inherits a plan id from the environment; pin it either way.
        row = {k: v for k, v in dict(proof, patch=patch_ref, patch_sha256=sha).items() if k != "plan_id"}
        return attempt, dict(row, plan_id=plan_id) if plan_id else row
    def land(**extra):
        return dict(dict(kind="patch-land", task_id="", patch_sha256=sha, exit=0, verify_exit=0, ts=stamp), **extra)
    def run(attempt, rows, parents=(), **kwargs):
        kwargs.setdefault("standalone_mode", True)
        kwargs.setdefault("aged_days", 7)
        return review_settlement._settle(project, [attempt], rows, set(parents), {}, **kwargs)
    landed, landed_delegate = fixture("settlement-landed-task")
    # Dry run in every standalone selection mode lists it as the accepted-patch outcome.
    for mode in ({}, {"aged_days": None, "standalone_older_than": 1}):
        preview = run(landed, [landed_delegate, land()], **mode)
        assert [r["attempt"] for r in preview["candidates"]] == [landed["attempt_id"]], preview
        row = preview["candidates"][0]
        assert (row["target_state"], row["reason_code"], row["selection_reason"]) == \
            ("done", "owner_accepted", "landed_patch"), preview
        assert not preview["applied"] and state_of(landed["attempt_id"]) == "review", preview
    # Owner mode never widens to a foreign standalone attempt because its bytes landed.
    foreign = run(landed, [landed_delegate, land()], parents={"owner-main"}, standalone_mode=False, aged_days=None)
    assert not foreign["candidates"] and not foreign["pending"] and not foreign["applied"], foreign
    # Only a successful, verified patch-land of the same task and bytes counts.
    for refused in (land(kind="patch-admit"), land(exit=1),
                    land(verify_exit=1), land(patch_sha256=other_sha), land(exit=None),
                    land(kind="delegate"), land(task_id="another-task")):
        report = run(landed, [landed_delegate, refused])
        assert not [c for c in report["candidates"] if c["selection_reason"] == "landed_patch"] \
            and not report["applied"], (refused, report)
    unverified = land()
    del unverified["verify_exit"]
    assert not [c for c in run(landed, [landed_delegate, unverified])["candidates"]
                if c["selection_reason"] == "landed_patch"]
    assert run(landed, [landed_delegate, land(task_id=landed["task_id"])])["candidates"][0]["selection_reason"] == "landed_patch"
    # A patch file that no longer hashes to the recorded bytes is no landing evidence.
    assert not [c for c in run(landed, [dict(landed_delegate, patch="missing.patch"), land()])["candidates"]
                if c["selection_reason"] == "landed_patch"]
    # Landing rows older than review entry cannot decide a re-entered review.
    stale = run(landed, [landed_delegate, land(ts="2000-01-01T00:00:00Z")])
    assert not stale["candidates"] and stale["pending"][0]["pending_reason"] == "newer_than_decision", stale
    # A failed delegate exit never matches, even when its bytes landed.
    assert not run(landed, [dict(landed_delegate, exit=1), land()])["candidates"]
    # Plan-bound delegates need the landing row's plan to match.
    def landed_only(report):
        return [c for c in report["candidates"] if c["selection_reason"] == "landed_patch"]
    bound, bound_delegate = fixture("settlement-landed-plan-task", "plan_" + "1" * 32)
    assert not landed_only(run(bound, [bound_delegate, land()]))
    assert not landed_only(run(bound, [bound_delegate, land(plan_id="plan_" + "2" * 32)]))
    assert landed_only(run(bound, [bound_delegate, land(plan_id="plan_" + "1" * 32)]))
    # Different bytes or a failed landing never settle as done.
    pending, pending_delegate = fixture("settlement-unlanded-task")
    for rows in ([pending_delegate, land(patch_sha256=other_sha)], [pending_delegate, land(exit=1)], [pending_delegate]):
        assert not landed_only(run(pending, rows)), rows
    assert state_of(pending["attempt_id"]) == "review"
    # Apply moves it to done with the owner-accepted reason; a rerun is a no-op.
    report = run(landed, [landed_delegate, land()], apply=True)
    assert report["applied"] == [landed["attempt_id"]] and not report["errors"], report
    final = json.loads(call(event_command + ["show", "--attempt", landed["attempt_id"], "--json"]))
    assert final["state"] == "done" and final["reason_code"] == "owner_accepted", final
    assert not run(dict(landed, state="done"), [landed_delegate, land()], apply=True)["applied"]
    call(event_command + ["transition", "--attempt", pending["attempt_id"], "--state", "cancelled"])
    call(event_command + ["transition", "--attempt", bound["attempt_id"], "--state", "cancelled"])
    # A parentless blocked attempt stays out of reach, as before.
    blocked = settlement_worker("settlement-standalone-blocked-task", legacy=True, review=False, exit_proof=False)
    settlement_transition(blocked, "blocked")
    attempts, _ = saved._records(project, full=True)
    blocked_attempt = next(a for a in attempts if a["attempt_id"] == blocked)
    assert not run(blocked_attempt, [])["candidates"] and state_of(blocked) == "blocked"
    call(event_command + ["transition", "--attempt", blocked, "--state", "cancelled"])
exercise_landed_standalone_review()

for extra in (["--legacy"], ["--legacy-older-than", "1"], ["--standalone"], ["--older-than", "1"],
              ["--standalone", "--older-than", "nan"], ["--standalone", "--older-than", "inf"],
              ["--standalone", "--older-than", "0"], ["--standalone", "--older-than", "-1"],
              ["--standalone", "--older-than", "1", "--owner-attempt", legacy_main],
              ["--owner-attempt", legacy_main, "--legacy", "--legacy-older-than", "nan"],
              ["--aged-days", "7"], ["--standalone", "--aged-days", "6.99"],
              ["--standalone", "--aged-days", "nan"], ["--standalone", "--aged-days", "inf"],
              ["--standalone", "--aged-days", "7", "--older-than", "1"],
              ["--owner-attempt", legacy_main, "--legacy", "--aged-days", "7", "--attempt", recent_review,
               "--reason", "cannot bypass age"]):
    refused = subprocess.run(recover[:recover.index("--owner-attempt")] + extra,
                             env=environment, capture_output=True)
    assert refused.returncode, ("invalid settlement selection accepted", extra, refused.stdout)
assert subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--aged-days", "7"],
                      env=environment, capture_output=True).returncode
del external_plan["tasks"][dead_task]
write_external_plan()
complete = saved.completed_tasks(project, ["elsewhere-task"])
assert complete == {"elsewhere-task": "completed"}, complete
from room_view import plan_rows, goal_box
plan_report = {"plan": {"present": True, "goal": "Elsewhere work", "task_count": 1, "by_state": {"review": 1}},
               "room": {"repo_tasks": list(external_plan["tasks"].values())}, "completed_tasks": complete}
assert any("completed locally" in row[0] for row in plan_rows(plan_report, {}, 120, True))
assert "1 locally completed" in "\n".join(row[0] for row in goal_box(plan_report, 120, True, False))
assert external_plan["tasks"]["elsewhere-task"]["state"] == "review", "display granted acceptance"
task_navigation = {"room_id": "elsewhere-room"}
with patch.object(panel.room, "status", return_value={"id": "elsewhere-room", "participants": []}):
    panel.navigate(project, ("task", "elsewhere-task"), plan_report, task_navigation, 120)
assert "Work: completed locally" in task_navigation["detail"]["text"], task_navigation
assert "Plan acceptance: review" in task_navigation["detail"]["text"], task_navigation
external_plan["tasks"]["elsewhere-task"]["state"] = "running"
write_external_plan()
assert not saved.completed_tasks(project, ["elsewhere-task"]), "reopened task inherited completion"
with patch.object(review_settlement, "_stopped", return_value=True):
    stale = review_settlement._settle(project, [new_worker], [], {main_attempts["codex"]},
        {"elsewhere-task": (decision, result_row)}, apply=False)
assert stale["pending"][0]["pending_reason"] == "stale_generation", stale
plan_path.write_text("{unreadable", encoding="utf-8")
with patch.object(review_settlement, "_stopped", return_value=True):
    unreadable = review_settlement._settle(project, [old_worker], [], {main_attempts["codex"]},
        {"elsewhere-task": (decision, result_row)}, apply=False)
assert unreadable["pending"][0]["pending_reason"] == "plan_unreadable", unreadable
external_plan["tasks"]["elsewhere-task"]["state"] = "review"
external_plan["plan_id"] = "plan_replaced"
write_external_plan()
assert not saved.completed_tasks(project, ["elsewhere-task"]), "replacement plan inherited completion"
external_plan["plan_id"] = "plan_elsewhere"
write_external_plan()
proof = (project / "value.txt").read_bytes()
(project / "value.txt").write_bytes(b"changed evidence\n")
assert not saved.completed_tasks(project, ["elsewhere-task"]), "changed evidence stayed completed"
(project / "value.txt").write_bytes(proof)
unverified_args = outside_args[:outside_args.index("--verify")]
call(unverified_args)
assert not saved.completed_tasks(project, ["elsewhere-task"]), "unverified latest result stayed completed"
with patch.object(saved, "_plan_bindings", side_effect=[
        {"elsewhere-task": outside["plan_binding"]}, {"elsewhere-task": {"plan_id": "changed"}}]):
    try:
        saved.finalize(project, "codex", "elsewhere-task", str(summary), "completed", "true")
    except ValueError as error:
        assert "plan task changed" in str(error), error
    else:
        raise AssertionError("task changed during verification but result was persisted")
if before_plan is None:
    plan_path.unlink()
else:
    plan_path.write_bytes(before_plan)
reopened = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                            "--task-id", "panel-council-test", "--json"]))
# A main that names itself (as Pi mains do) reads the same results.
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results", "--owner", "codex",
                        "--task-id", "panel-council-test", "--json"])) == reopened
row = reopened["rows"][0]
assert row["acceptance"] and row["summary"].startswith("결과 화면") and row["delivery"] == "not_requested"
assert any(c.get("answer", "").startswith("Answer:") for c in row["calls"]), row
assert all("--prompt" not in c.get("answer", "") for c in row["calls"])
result_layout = deepcopy(reopened)
result_layout["rows"][0]["calls"][-1]["changes"] = {"count": 3, "added": 4, "removed": 2,
    "files": [["one.py", 2, 1], ["two.py", 1, 1], ["three.py", 1, 0]]}
long_code = '  print("' + 'A' * 160 + '")'
long_table = '| ' + 'B' * 160 + ' | tail |'
wide_code = '  ' + '한글☀️e\u0301' * 60
retained_code = '  ' + 'C' * 2100 + '  end  '

# Each row declares its block kind and the exact displayed prose, or None for
# filtered machine prose. Code/table rows must reconstruct the original bytes.
def p(line, displayed=None):
    return (line, 'prose', line if displayed is None else displayed)


def c(line):
    return (line, 'code', line)


def t(line):
    return (line, 'table', line)


cases = [
    ('fenced long/literal/machine/control lines', [c(line) for line in (
        '```py', '  x = 1', '    y = 2', long_code, '    `literal`  **token**  ',
        '> literal code ' + 'D' * 160, wide_code, retained_code, '~~~',
        'stop-reason: literal-data', 'model-route: literal-data', '\tmodel-route: tabbed',
        'control\x1b[31m\x00', '```')]),
    ('long table row', [t(line) for line in ('| a | b |', '| --- | --- |', long_table)]),
    ('indented literal and tab code', [c(line) for line in (
        '    indented  text', '    foo__bar = "__"  **token**  `literal`  ' + 'E' * 160 + '  ',
        '\tfoo__bar = "__"  **token**  `literal`  ' + 'F' * 160 + '  ',
        '    model-route: literal-data', '    stop-reason: literal-data')]),
    ('machine prose', [('stop-reason: machine', 'prose', None),
                       ('  model-route: machine', 'prose', None), p('**visible**', 'visible')]),
    ('list then top-level fence closes list', [p('- item'), c('```'), c('fenced'), c('```'),
        c('    foo__bar = `literal`'), c('    model-route: literal-data')]),
    ('blank/dedent closes list', [p('- item'), p(''), p('**outside**', 'outside'), p(''),
        c('    **literal**'), c('    model-route: literal-data')]),
    ('six-space backticks stay paragraph text', [p('- item'),
        p('      ```', '      '), p('  **continuation**', '  continuation'),
        ('  model-route: prose', 'prose', None)]),
    ('six-space backticks after blank are indented code', [p('- item'), p(''),
        c('      ```'), c('      foo__bar = `literal` **token**'), c('      ```'),
        p('  **continuation**', '  continuation')]),
    ('four-space backticks cannot close fence', [c('```'), c('    ```'),
        c('model-route: literal-data'), c('```'), p('**outside**', 'outside')]),
    ('list starts with fence', [c('- ```py'), c('  foo__bar = `literal` **token**'), c('  ```'),
        p('    **continuation**', '    continuation'), ('    model-route: prose', 'prose', None)]),
    ('nested lists/lazy continuation', [p('- item'), p('  - nested'),
        p('**lazy**', 'lazy'), p(''), c('        - foo__bar = `literal` **token**'),
        p('  **parent**', '  parent'), p(''), c('      parent__code = `literal`')]),
    ('indented code cannot interrupt paragraph', [p('**paragraph**', 'paragraph'),
        p('    **literal**', '    literal'),
        ('    model-route: prose', 'prose', None)]),
    ('fence closes at container loss', [c('- ~~~'), c('  literal'), p('**outside**', 'outside'),
        p('    **paragraph**', '    paragraph')]),
    ('fence run/closing suffix rules', [c('````py'), c('```'), c('~~~~'), c('```` suffix'),
        c('   `````  '), p('**outside**', 'outside')]),
    ('invalid backtick info string is prose', [p('```py`bad', 'pybad'),
        p('    **paragraph**', '    paragraph')]),
    ('pipe rows without delimiter are prose', [p('| **header** | `value` |', '| header | value |'),
        p('| __row__ | value |', '| row | value |')]),
    ('delimiter cell count mismatch is prose', [p('| **a** | b |', '| a | b |'), p('| --- |'),
        p('| __row__ |', '| row |')]),
    ('delimiter alone is prose', [p('| --- | --- |'), p('| **row** | b |', '| row | b |')]),
    ('ordered interruption requires one', [p('paragraph'), p('10. item'), p(''),
        c('    foo__bar = `literal`')]),
    ('ordered leading-zero interruption', [p('paragraph'), p('01. item'), p(''),
        p('    **item**', '    item'), ('    model-route: prose', 'prose', None)]),
    ('marker padding starts code', [c('-     foo__bar = `literal`'), c('      **literal**')]),
    ('empty list item content column', [p('-'), c('  ```'), c('  foo__bar'), c('  ```'),
        p('  **continuation**', '  continuation')]),
    ('ordered sibling starts fence', [p('1. first'), c('2. ~~~'),
        c('   model-route: literal'), c('   ~~~')]),
    ('empty item ends at extra blank (CommonMark 280)', [p('-'), p(''),
        c('    model-route: literal')]),
    ('pipe-free body row (GFM 202)', [t('| a | b |'), t('| --- | --- |'),
        t('model-route: literal'), p(''), ('model-route: prose', 'prose', None)]),
    ('one-cell delimiter without pipe', [t('| a |'), t('---'), t('model-route: literal')]),
    ('quoted list fence', [c('> 1. ~~~'), c('>    model-route: literal'), c('>    ~~~'),
        p(''), c('    model-route: outside')]),
    ('quoted indented chunks (CommonMark 111)', [c('>     **literal**'), c('> '), c('>'),
        c('>     model-route: literal'), p(''), p('outside')]),
    ('quoted table', [t('> | a | b |'), t('> | --- | --- |'), t('> model-route: literal'),
        p(''), ('model-route: prose', 'prose', None)]),
    ('lazy list text cannot start table', [p('- first'), p('| a | b |'), p('| --- | --- |'),
        ('model-route: prose', 'prose', None)]),
    ('thematic break is not list item', [p('- - -'), c('    model-route: literal')]),
    # Official block examples; rendering retains source rather than emitting HTML.
    ('CommonMark 0.31.2 example 107', [
        c('    a simple'), c('      indented code block')]),
    ('CommonMark 0.31.2 example 108', [
        p('  - foo'), p(''), p('    bar')]),
    ('CommonMark 0.31.2 example 109', [
        p('1.  foo'), p(''), p('    - bar')]),
    ('CommonMark 0.31.2 example 110', [
        c('    <a/>'), c('    *hi*'), c(''), c('    - one')]),
    ('CommonMark 0.31.2 example 111', [
        c('    chunk1'), c(''), c('    chunk2'), c('  '), c(' '), c(' '), c('    chunk3')]),
    ('CommonMark 0.31.2 example 112', [
        c('    chunk1'), c('      '), c('      chunk2')]),
    ('CommonMark 0.31.2 example 113', [
        p('Foo'), p('    bar'), p('')]),
    ('CommonMark 0.31.2 example 114', [
        c('    foo'), p('bar')]),
    ('CommonMark 0.31.2 example 115', [
        p('# Heading', 'Heading'), c('    foo'), p('Heading'), p('------'), c('    foo'), p('----')]),
    ('CommonMark 0.31.2 example 116', [
        c('        foo'), c('    bar')]),
    ('CommonMark 0.31.2 example 117', [
        p(''), p('    '), c('    foo'), p('    '), p('')]),
    ('CommonMark 0.31.2 example 118', [
        c('    foo  ')]),
    ('CommonMark 0.31.2 example 119', [
        c('```'), c('<'), c(' >'), c('```')]),
    ('CommonMark 0.31.2 example 120', [
        c('~~~'), c('<'), c(' >'), c('~~~')]),
    ('CommonMark 0.31.2 example 121', [
        p('``', ''), p('foo'), p('``', '')]),
    ('CommonMark 0.31.2 example 122', [
        c('```'), c('aaa'), c('~~~'), c('```')]),
    ('CommonMark 0.31.2 example 123', [
        c('~~~'), c('aaa'), c('```'), c('~~~')]),
    ('CommonMark 0.31.2 example 124', [
        c('````'), c('aaa'), c('```'), c('``````')]),
    ('CommonMark 0.31.2 example 125', [
        c('~~~~'), c('aaa'), c('~~~'), c('~~~~')]),
    ('CommonMark 0.31.2 example 126', [
        c('```')]),
    ('CommonMark 0.31.2 example 127', [
        c('`````'), c(''), c('```'), c('aaa')]),
    ('CommonMark 0.31.2 example 128', [
        c('> ```'), c('> aaa'), p(''), p('bbb')]),
    ('CommonMark 0.31.2 example 129', [
        c('```'), c(''), c('  '), c('```')]),
    ('CommonMark 0.31.2 example 130', [
        c('```'), c('```')]),
    ('CommonMark 0.31.2 example 131', [
        c(' ```'), c(' aaa'), c('aaa'), c('```')]),
    ('CommonMark 0.31.2 example 132', [
        c('  ```'), c('aaa'), c('  aaa'), c('aaa'), c('  ```')]),
    ('CommonMark 0.31.2 example 133', [
        c('   ```'), c('   aaa'), c('    aaa'), c('  aaa'), c('   ```')]),
    ('CommonMark 0.31.2 example 134', [
        c('    ```'), c('    aaa'), c('    ```')]),
    ('CommonMark 0.31.2 example 135', [
        c('```'), c('aaa'), c('  ```')]),
    ('CommonMark 0.31.2 example 136', [
        c('   ```'), c('aaa'), c('  ```')]),
    ('CommonMark 0.31.2 example 137', [
        c('```'), c('aaa'), c('    ```')]),
    ('CommonMark 0.31.2 example 138', [
        p('``` ```', ''), p('aaa')]),
    ('CommonMark 0.31.2 example 139', [
        c('~~~~~~'), c('aaa'), c('~~~ ~~')]),
    ('CommonMark 0.31.2 example 140', [
        p('foo'), c('```'), c('bar'), c('```'), p('baz')]),
    ('CommonMark 0.31.2 example 141', [
        p('foo'), p('---'), c('~~~'), c('bar'), c('~~~'), p('# baz', 'baz')]),
    ('CommonMark 0.31.2 example 142', [
        c('```ruby'), c('def foo(x)'), c('  return 3'), c('end'), c('```')]),
    ('CommonMark 0.31.2 example 143', [
        c('~~~~    ruby startline=3 $%@#$'), c('def foo(x)'), c('  return 3'), c('end'), c('~~~~~~~')]),
    ('CommonMark 0.31.2 example 144', [
        c('````;'), c('````')]),
    ('CommonMark 0.31.2 example 145', [
        p('``` aa ```', 'aa'), p('foo')]),
    ('CommonMark 0.31.2 example 146', [
        c('~~~ aa ``` ~~~'), c('foo'), c('~~~')]),
    ('CommonMark 0.31.2 example 147', [
        c('```'), c('``` aaa'), c('```')]),
    ('CommonMark 0.31.2 example 253', [
        p('A paragraph'), p('with two lines.'), p(''), c('    indented code'), p(''), p('> A block quote.')]),
    ('CommonMark 0.31.2 example 254', [
        p('1.  A paragraph'), p('    with two lines.'), p(''), c('        indented code'), p(''),
        p('    > A block quote.')]),
    ('CommonMark 0.31.2 example 255', [
        p('- one'), p(''), p(' two')]),
    ('CommonMark 0.31.2 example 256', [
        p('- one'), p(''), p('  two')]),
    ('CommonMark 0.31.2 example 257', [
        p(' -    one'), p(''), c('     two')]),
    ('CommonMark 0.31.2 example 258', [
        p(' -    one'), p(''), p('      two')]),
    ('CommonMark 0.31.2 example 259', [
        p('   > > 1.  one'), p('>>'), p('>>     two')]),
    ('CommonMark 0.31.2 example 260', [
        p('>>- one'), p('>>'), p('  >  > two')]),
    ('CommonMark 0.31.2 example 261', [
        p('-one'), p(''), p('2.two')]),
    ('CommonMark 0.31.2 example 262', [
        p('- foo'), p(''), p(''), p('  bar')]),
    ('CommonMark 0.31.2 example 263', [
        p('1.  foo'), p(''), c('    ```'), c('    bar'), c('    ```'), p(''), p('    baz'), p(''),
        p('    > bam')]),
    ('CommonMark 0.31.2 example 264', [
        p('- Foo'), p(''), c('      bar'), c(''), c(''), c('      baz')]),
    ('CommonMark 0.31.2 example 265', [
        p('123456789. ok')]),
    ('CommonMark 0.31.2 example 266', [
        p('1234567890. not ok')]),
    ('CommonMark 0.31.2 example 267', [
        p('0. ok')]),
    ('CommonMark 0.31.2 example 268', [
        p('003. ok')]),
    ('CommonMark 0.31.2 example 269', [
        p('-1. not ok')]),
    ('CommonMark 0.31.2 example 270', [
        p('- foo'), p(''), c('      bar')]),
    ('CommonMark 0.31.2 example 271', [
        p('  10.  foo'), p(''), c('           bar')]),
    ('CommonMark 0.31.2 example 272', [
        c('    indented code'), p(''), p('paragraph'), p(''), c('    more code')]),
    ('CommonMark 0.31.2 example 273', [
        c('1.     indented code'), p(''), p('   paragraph'), p(''), c('       more code')]),
    ('CommonMark 0.31.2 example 274', [
        c('1.      indented code'), p(''), p('   paragraph'), p(''), c('       more code')]),
    ('CommonMark 0.31.2 example 275', [
        p('   foo'), p(''), p('bar')]),
    ('CommonMark 0.31.2 example 276', [
        p('-    foo'), p(''), p('  bar')]),
    ('CommonMark 0.31.2 example 277', [
        p('-  foo'), p(''), p('   bar')]),
    ('CommonMark 0.31.2 example 278', [
        p('-'), p('  foo'), p('-'), c('  ```'), c('  bar'), c('  ```'), p('-'), c('      baz')]),
    ('CommonMark 0.31.2 example 279', [
        p('-   ', '-'), p('  foo')]),
    ('CommonMark 0.31.2 example 280', [
        p('-'), p(''), p('  foo')]),
    ('CommonMark 0.31.2 example 281', [
        p('- foo'), p('-'), p('- bar')]),
    ('CommonMark 0.31.2 example 282', [
        p('- foo'), p('-   ', '-'), p('- bar')]),
    ('CommonMark 0.31.2 example 283', [
        p('1. foo'), p('2.'), p('3. bar')]),
    ('CommonMark 0.31.2 example 284', [
        p('*')]),
    ('CommonMark 0.31.2 example 285', [
        p('foo'), p('*'), p(''), p('foo'), p('1.')]),
    ('CommonMark 0.31.2 example 286 (short prose)', [
        p(' 1.  A paragraph'), p('     two lines.'), p(''), c('         indented code'), p(''),
        p('     > quote.')]),
    ('CommonMark 0.31.2 example 287 (short prose)', [
        p('  1.  A paragraph'), p('      two lines.'), p(''), c('          indented code'), p(''),
        p('      > quote.')]),
    ('CommonMark 0.31.2 example 288 (short prose)', [
        p('   1.  A paragraph'), p('       two lines.'), p(''), c('           indented code'), p(''),
        p('       > quote.')]),
    ('CommonMark 0.31.2 example 289', [
        c('    1.  A paragraph'), c('        with two lines.'), c(''), c('            indented code'), c(''),
        c('        > A block quote.')]),
    ('CommonMark 0.31.2 example 290 (short prose)', [
        p('  1.  A paragraph'), p('two lines.'), p(''), c('          indented code'), p(''),
        p('      > quote.')]),
    ('CommonMark 0.31.2 example 291', [
        p('  1.  A paragraph'), p('    with two lines.')]),
    ('CommonMark 0.31.2 example 292', [
        p('> 1. > Blockquote'), p('continued here.')]),
    ('CommonMark 0.31.2 example 293', [
        p('> 1. > Blockquote'), p('> continued here.')]),
    ('CommonMark 0.31.2 example 294', [
        p('- foo'), p('  - bar'), p('    - baz'), p('      - boo')]),
    ('CommonMark 0.31.2 example 295', [
        p('- foo'), p(' - bar'), p('  - baz'), p('   - boo')]),
    ('CommonMark 0.31.2 example 296', [
        p('10) foo'), p('    - bar')]),
    ('CommonMark 0.31.2 example 297', [
        p('10) foo'), p('   - bar')]),
    ('CommonMark 0.31.2 example 298', [
        p('- - foo')]),
    ('CommonMark 0.31.2 example 299', [
        p('1. - 2. foo')]),
    ('CommonMark 0.31.2 example 300', [
        p('- # Foo'), p('- Bar'), p('  ---'), p('  baz')]),
    ('GFM example 198', [
        t('| foo | bar |'), t('| --- | --- |'), t('| baz | bim |')]),
    ('GFM example 199', [
        t('| abc | defghi |'), t(':-: | -----------:'), t('bar | baz')]),
    ('GFM example 200', [
        t('| f\\|oo  |'), t('| ------ |'), t('| b `\\|` az |'), t('| b **\\|** im |')]),
    ('GFM example 201', [
        t('| abc | def |'), t('| --- | --- |'), t('| bar | baz |'), p('> bar')]),
    ('GFM example 202', [
        t('| abc | def |'), t('| --- | --- |'), t('| bar | baz |'), t('bar'), p(''), p('bar')]),
    ('GFM example 203', [
        p('| abc | def |'), p('| --- |'), p('| bar |')]),
    ('GFM example 204', [
        t('| abc | def |'), t('| --- | --- |'), t('| bar |'), t('| bar | baz | boo |')]),
    ('GFM example 205', [
        t('| abc | def |'), t('| --- | --- |')]),
]
for terminator in ('# heading | tail', '---', '* * *', '> quote | tail', '- item | tail',
                   '2. item | tail', '-', '2.', '<div> | tail', '<!-- | tail -->',
                   '<!DOCTYPE> | tail', '<a>', '<a title="x">', '</a>'):
    cases.append(('table block boundary %s' % terminator, [t('| a | b |'), t('| --- | --- |'),
        p(terminator, 'heading | tail' if terminator.startswith('#') else terminator),
        p(''), ('model-route: prose', 'prose', None)]))
for row in ('<a> | tail', '<a bad=>', '<!doctype> | tail', '<script/>'):
    cases.append(('inline HTML stays table row %s' % row, [t('| a | b |'), t('| --- | --- |'), t(row)]))
for first, marker, indent in (('1. first', '20.', '    '), ('- first', '2)', '   '),
                              ('- first\n  1. nested', '  2.', '     ')):
    cases.append(('ordered sibling %s/%s' % (first, marker), [p(line) for line in first.split('\n')] +
        [c(marker + ' ~~~'), c(indent + 'model-route: literal'), c(indent + '~~~')]))
for marker, indent in (('-', '    '), ('1.', '    '), ('-\n  -', '      '), ('> -', '>     ')):
    cases.append(('empty item blank boundary %s' % marker, [p(line) for line in marker.split('\n')] +
        [p('> ', '>') if marker.startswith('>') else p(''), c(indent + 'model-route: literal')]))
for item, column in (('- item', 2), ('10. item', 4), ('- item\n  - nested', 4)):
    prefix = [p(line) for line in item.split('\n')]
    for leading in (column, column + 3):
        spaces = ' ' * leading
        cases.append(('blank-separated item paragraph %s/%s' % (item, leading), prefix + [p(''),
            p(spaces + '**paragraph**', spaces + 'paragraph'), (spaces + 'model-route: prose', 'prose', None)]))
    spaces = ' ' * (column + 4)
    cases.append(('item indented code %s' % item, prefix + [p(''),
        c(spaces + '**literal** ' + 'G' * 160), c(spaces + 'model-route: literal-data')]))
    for fence in ('```', '~~~'):
        for gap in ([], [p('')]):
            cases.append(('nested fence %s/%s/%s' % (item, fence, bool(gap)), prefix + gap +
                [c(' ' * column + fence + 'text'), c(''), c(' ' * column + 'foo__bar'),
                 c(' ' * column + fence), p(' ' * column + '**continuation**', ' ' * column + 'continuation'),
                 (' ' * column + 'model-route: prose', 'prose', None)]))
for left, right in (('| ', ' |'), ('', ''), ('| ', ''), ('', ' |')):
    table_lines = [left + row + right for row in (
        'header | value', ':--- | ---:', 'a | b', '**literal** | ' + '한글' * 80 + '  end  ',
        'model-route: literal-data | tail', r'escaped\|pipe | tail')]
    cases.append(('edge pipes %r/%r' % (left, right), [t(line) for line in table_lines]))
    cases.append(('nested table %r/%r' % (left, right), [p('- item')] +
        [t('  ' + line) for line in table_lines] + [t('    **continuation**')]))

from panel_view import _result_blocks
aggregate_results = {'rows': [{'task_id': 'aggregate-first', 'summary': 'FIRST_TASK_BODY'},
                              {'task_id': 'aggregate-second', 'summary': 'SECOND_TASK_BODY'}]}
for compact_results in (False, True):
    aggregate_text = render_results(aggregate_results, 100, compact=compact_results)
    assert 'aggregate-first' in aggregate_text and 'aggregate-second' in aggregate_text, aggregate_text
aggregate_out = io.StringIO()
with patch.object(panel, 'results', return_value=aggregate_results), \
        patch.object(panel.sys, 'stdout', aggregate_out):
    assert panel.main(['--repo', str(project), '--results']) == 0
assert 'FIRST_TASK_BODY' in aggregate_out.getvalue() and 'SECOND_TASK_BODY' in aggregate_out.getvalue()
token_calls = {'rows': [{'task_id': 'token-result', 'summary': 'Recorded result', 'outcome': 'completed',
                         'calls': [{'kind': 'delegate', 'exit': 0, 'state': 'done', 'attempt_id': 'token-call',
                                    'served_model': 'gpt-6-luna', 'tokens': 1820000,
                                    'provider_usage': {'input_tokens': 1700000, 'output_tokens': 120000,
                                                       'cache_in_input': True}},
                                   {'kind': 'call', 'exit': 0, 'state': 'done',
                                    'served_model': 'gpt-6-luna', 'tokens': 1820000},
                                   {'kind': 'review', 'exit': 0, 'state': 'done', 'attempt_id': 'no-usage',
                                    'served_model': 'claude-opus-5-5'}]}]}
token_text = render_results(token_calls, 160)
assert 'Luna 6 / delegate / exit 0 / 1.7m in / 120.0k out' in token_text, token_text
assert 'Luna 6 / call / exit 0 / 1.8m tok' in token_text, token_text
assert 'Opus 5.5 / review / exit 0' in token_text and 'Opus 5.5 / review / exit 0 /' not in token_text, token_text
from dashboard_projection import ELLIPSIS, use_unicode
import unicodedata
previous_unicode = ELLIPSIS[0] == '…'
for glyphs in (True, False):
    use_unicode(glyphs)
    marker = '→ ' if glyphs else '-> '
    for width in (24, 42, 50, 80, 100, 127):
        result_text = render_results(result_layout, width, heading='Worker Sol 6.1 · finished')
        assert '\033' not in result_text and all(display_width(line) <= width for line in result_text.splitlines())
        assert 'Outcome:' in result_text and '결과 화면' in result_text and 'Changed files: 3' in result_text
        assert 'not_requested' not in result_text, result_text
        assert result_text.index('Changed files:') < result_text.index('결과 화면'), result_text
        for name, fixture in cases:
            source = '\n'.join(row[0] for row in fixture)
            assert list(_result_blocks(source)) == [(row[0], row[1]) for row in fixture], name
            for field, indent in (('summary', '  '), ('answer', '    ')):
                row = {field: source} if field == 'summary' else {'summary': 'BODY', 'calls': [{'answer': source}]}
                rendered = render_results({'rows': [row]}, width).split('\n')
                assert all(display_width(line.expandtabs(8)) <= width for line in rendered), (name, width, glyphs, rendered)
                reference = {field: 'BODY'} if field == 'summary' else {'summary': 'BODY', 'calls': [{'answer': 'BODY'}]}
                reference_rows = render_results({'rows': [reference]}, width).split('\n')
                start = max(i for i, line in enumerate(reference_rows) if line == indent + 'BODY')
                body = rendered[start:-2]
                cursor = 0
                for original, kind, expected in fixture:
                    if kind == 'prose':
                        if expected is None or (original.strip() and not expected.strip()):
                            continue
                        expected = indent + expected if original.strip() else ''
                        assert body[cursor] == expected, (name, field, width, original, body[cursor], expected)
                        cursor += 1
                    else:
                        expected = ''.join('?' if char != '\t' and unicodedata.category(char)[0] == 'C' else char
                                           for char in original)
                        assert body[cursor].startswith(indent), (name, body[cursor])
                        restored = body[cursor][len(indent):]
                        cursor += 1
                        while len(restored) < len(expected):
                            assert body[cursor].startswith(indent + marker), (name, body[cursor])
                            restored += body[cursor][len(indent + marker):]
                            cursor += 1
                        assert restored == expected, (name, field, width, restored, expected)
                assert cursor == len(body), (name, field, body[cursor:])
use_unicode(previous_unicode)
failure = subprocess.run(final_args[:-4] + ["--verify", "false", "--evidence", "value.txt"],
                         cwd=project, env=environment, capture_output=True, text=True)
assert failure.returncode != 0 and "verifier failed" in failure.stderr
verify_failure = list(final_args)
verify_failure[verify_failure.index("--verify") + 1] = "printf verifier-out; printf verifier-err >&2; exit 7"
failed_verify = subprocess.run(verify_failure, cwd=project, env=environment, capture_output=True, text=True)
assert failed_verify.returncode and "exit 7" in failed_verify.stderr, failed_verify.stderr
def private_diagnostics(error):
    relative = re.search(r"private diagnostics: (\.oms/artifacts/panel-results/verify-[a-f0-9]+\.json)", error).group(1)
    path = project / relative
    if os.name != "nt":
        assert path.stat().st_mode & 0o077 == 0, oct(path.stat().st_mode)
    return json.loads(path.read_text())
diagnostic = private_diagnostics(failed_verify.stderr)
assert diagnostic["stdout"]["text"] == "verifier-out" and diagnostic["stderr"]["text"] == "verifier-err", diagnostic
assert "verifier-out" not in failed_verify.stderr and "verifier-err" not in failed_verify.stderr
with patch.dict(os.environ, environment, clear=True):
    noisy = saved._run_verifier(project, "printf '%20000s' x; printf '%20000s' y >&2; exit 8", 5)
    diagnostic = json.loads((project / noisy["diagnostics"]["path"]).read_text())
    assert noisy["exit"] == 8 and saved._relative(project, noisy["diagnostics"]["path"]) == noisy["diagnostics"]
    assert all(diagnostic[name]["truncated"] and diagnostic[name]["complete"]
               and diagnostic[name]["bytes"] == 20000
               and len(diagnostic[name]["text"].encode()) == saved.MAX_VERIFY_OUTPUT
               for name in ("stdout", "stderr")), diagnostic
    failed_result = saved.finalize(project, "codex", "verifier-completed-failure", str(summary), "completed", "exit 9")
    assert failed_result["verification"]["exit"] == 9 and failed_result["verification"]["diagnostics"]["path"]
write_finalize = list(final_args)
write_finalize[write_finalize.index("--task-id") + 1] = "panel-route-3"
failure = subprocess.run(write_finalize, cwd=project, env=environment, capture_output=True, text=True)
assert failure.returncode != 0 and "matching landed patch" in failure.stderr
whole_finalize = list(final_args)
whole_finalize[whole_finalize.index("--task-id") + 1] = "whole-main-result"
whole_finalize[whole_finalize.index("--owner") + 1] = "claude"
owner_env = dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"])
whole_unlanded = json.loads(call(whole_finalize, owner_env))
assert whole_unlanded["outcome"] == "accepted" and not whole_unlanded["admission_event_ids"]
call(["bash", str(panel.ENTRY), "patch-land", "--repo", str(project), "--patch", write_patch,
      "--verify", "test \"$(tail -1 value.txt)\" = new"])
write_finalize[write_finalize.index("--verify") + 1] = "test \"$(tail -1 value.txt)\" = new"
whole_finalize[whole_finalize.index("--verify") + 1] = "test \"$(tail -1 value.txt)\" = new"
unreviewed_finalize = subprocess.run(write_finalize, cwd=project, env=environment, capture_output=True, text=True)
assert unreviewed_finalize.returncode != 0 and "att_" in unreviewed_finalize.stderr \
    and "lack a passing review gate" in unreviewed_finalize.stderr, unreviewed_finalize.stderr
write_rows = saved._records(project)[1]
write_event = next(row for row in write_rows if row.get("kind") == "delegate"
                   and row.get("task_id") == "panel-route-3" and row.get("patch_sha256"))
review_artifact = project / ".oms/artifacts/fixture/panel-route-3-review.md"
review_artifact.parent.mkdir(parents=True, exist_ok=True)
review_artifact.write_text("## Output\nGATE: pass\n## Exit\n0\n", encoding="utf-8")
review_payload = json.dumps({"gate_verify_exit": 0,
                             "candidate": {"patch_sha256": write_event["patch_sha256"]},
                             "seats": [{"verdict": "pass", "artifact": review_artifact.name}]})
call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" review-outcome local 0 "$3"',
      "panel-review-fixture", str(root / "scripts/lib/peer-common.sh"), str(project), str(review_artifact)],
     dict(environment, OMS_TASK_ID="panel-route-3", OMS_ATTEMPT_ID="att_panel_route_3_review",
          OMS_INDEX_REVIEW_OUTCOME_JSON=review_payload))
assert json.loads(call(write_finalize))["outcome"] == "accepted"
# Finalize waits only for review sources that overlap its verifier checkout.
review_pass = project / ".oms/locks/canonical-sync.d/review-read-pass-fixture"
review_pass.parent.mkdir(parents=True, exist_ok=True)
review_owner_script = ('. "$1/scripts/lib/canonical-sync.sh"; '
                       'oms_canonical_call_acquire "$2" "$3"; '
                       'printf "ready\\n"; IFS= read -r release; oms_canonical_call_release')
review_owner = subprocess.Popen(
    ["bash", "-c", review_owner_script, "canonical-review-owner", str(root), str(project), review_pass.name],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
assert review_owner.stdout.readline().strip() == "ready"
malformed_review = project / ".oms/locks/canonical-sync.d/review-malformed-fixture"
deferred_args = list(final_args)
deferred_args[deferred_args.index("--task-id") + 1] = "finalize-review-read-pass"
deferred_args[deferred_args.index("--outcome") + 1] = "completed"
try:
    (review_pass / "source").write_text(str(execution_repo.resolve()) + "\n")
    unrelated = panel_with_review_wait(deferred_args[3:], 0, cwd=project, env=environment)
    assert unrelated.returncode == 0 and "waiting up to" not in unrelated.stderr, unrelated.stderr
    assert json.loads(unrelated.stdout)["verification"]["status"] == "passed"
    # Same-checkout and legacy entries both defer before any durable side effect.
    deferred_args[deferred_args.index("--task-id") + 1] = "finalize-review-conflict"
    protected_result_dirs = [project / ".oms" / name for name in ("artifacts", "lifecycle", "plan")]
    for source in (str(project.resolve()), None):
        if source is None:
            (review_pass / "source").unlink()
        else:
            (review_pass / "source").write_text(source + "\n")
        before_conflict = {path.relative_to(project): path.read_bytes()
                           for directory in protected_result_dirs for path in directory.rglob("*") if path.is_file()}
        conflict = panel_with_review_wait(deferred_args[3:], 0, cwd=project, env=environment)
        assert conflict.returncode == 75 and review_pass.name in conflict.stderr, conflict.stderr
        assert {path.relative_to(project): path.read_bytes()
                for directory in protected_result_dirs for path in directory.rglob("*")
                if path.is_file()} == before_conflict
    with patch.object(saved.subprocess, "check_output", side_effect=subprocess.CalledProcessError(1, "fixture")), \
            patch.object(saved, "time", Mock(wraps=saved.time, sleep=Mock(side_effect=AssertionError("inspection failure waited")))):
        try:
            saved.finalize(project, "codex", "registry-error", str(summary), "completed")
        except ValueError as error:
            assert "canonical-sync registry" in str(error) and "repair" in str(error), error
        else:
            raise AssertionError("registry inspection failure was accepted")
    malformed_review.mkdir()
    deferred = panel_with_review_wait(deferred_args[3:], 2, cwd=project, env=environment)
    assert deferred.returncode == 75 and \
        "review-read-pass-fixture" in deferred.stderr and "review-malformed-fixture" in deferred.stderr, deferred.stderr
    assert deferred.stderr.count("waiting up to") == 1, deferred.stderr
    assert not any(row.get("kind") == "panel-result" and row.get("task_id") == "finalize-review-conflict"
                   for row in saved._records(project)[1]), "deferred finalize wrote a result"
finally:
    review_owner.communicate("release\n", timeout=10)
    shutil.rmtree(malformed_review, ignore_errors=True)
# A review pass that ends during the bounded wait lets finalize proceed instead of exiting 75.
gap_owner = subprocess.Popen(
    ["bash", "-c", review_owner_script, "canonical-review-owner", str(root), str(project), review_pass.name],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
assert gap_owner.stdout.readline().strip() == "ready"
gap_args = list(deferred_args)
gap_args[gap_args.index("--task-id") + 1] = "finalize-review-gap"
gap_timer = threading.Timer(2, lambda: gap_owner.communicate("release\n", timeout=10))
gap_timer.start()
try:
    gap = panel_with_review_wait(gap_args[3:], 60, cwd=project, env=environment)
finally:
    gap_timer.join()
assert gap.returncode == 0 and gap.stderr.count("waiting up to") == 1 and "deferred" not in gap.stderr, \
    (gap.returncode, gap.stderr)
assert any(row.get("kind") == "panel-result" and row.get("task_id") == "finalize-review-gap"
           for row in saved._records(project)[1]), "finalize after the gap wrote no result"
# BEGIN reviewed-result acceptance composition (uses only disposable Git fixtures).
accept_repo = temporary / "accept state repo"
subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(project), str(accept_repo)], check=True)
for key, value in (("user.name", "accept fixture"), ("user.email", "accept@example.com")):
    subprocess.run(["git", "-C", str(accept_repo), "config", key, value], check=True)
accept_env = dict(environment, OMS_AGENT="codex", OMS_ROOM_REPO=str(accept_repo),
                  OMS_ROOM_ID="accept-room", OMS_ROOM_PARTICIPANT="accept-main")
for key in ("OMS_STATE_REPO", "OMS_PLAN_FILE", "OMS_PANEL_MAIN_ATTEMPT", "OMS_PANEL_EXECUTION_ROOT",
            "OMS_ATTEMPT_ID", "OMS_TASK_ID", "OMS_LEASE_ID", "OMS_PLAN_LEASE_ID"):
    accept_env.pop(key, None)
def accept_call(verb, *arguments, check=True, execution=None, env=None):
    options = ["--repo", str(execution or accept_repo)]
    command = (["bash", str(panel.ENTRY), verb, *arguments, *options] if verb == "scratch-worktree"
               else ["bash", str(panel.ENTRY), verb, *options, *arguments])
    result = subprocess.run(command,
                            env=env or accept_env, cwd=accept_repo, capture_output=True, text=True, timeout=90)
    if check:
        assert result.returncode == 0, (verb, arguments, result.stdout, result.stderr)
    return result
continue_error = "--continue requires a write-capable --dispatch role with --brief-file, and takes no --prompt or --task-id"
for continue_options in (
        ("--continue", "continued-task", "--owner", "claude", "--brief-file", "unused.md", "--access", "write"),
        ("--continue", "continued-task", "--brief-file", "unused.md", "--access", "write")):
    invalid_continue = accept_call("panel", *continue_options, execution=accept_repo, check=False)
    assert invalid_continue.returncode == 2 and continue_error in invalid_continue.stderr, invalid_continue.stderr
with patch.dict(os.environ, accept_env, clear=True):
    room.create(accept_repo, "accept-room", "Acceptance composition")
    room.join(accept_repo, "accept-room", "accept-main", "codex", "main")
accept_env["OMS_PANEL_MAIN_ATTEMPT"] = accept_call("agent-events", "start", "--provider", "codex",
    "--tool", "panel-main", "--ref", "panel_role=main", "--ref", "panel_room_id=accept-room",
    "--ref", "panel_room_participant=accept-main", "--then", "starting", "--then", "working").stdout.strip()
accept_call("agent-plan", "init", "--goal", "Review one worker result")
import shlex
accept_admission_hook = temporary / "accept-admission-hook.sh"
accept_verify = 'if [ -f %s ]; then bash %s; fi; test "$(tail -1 value.txt)" = accepted' % (
    shlex.quote(str(accept_admission_hook)), shlex.quote(str(accept_admission_hook)))
accept_call("agent-plan", "add", "--id", "accept-task", "--title", "Accept fixture",
            "--allowed", "value.txt,literal[*].txt", "--verify", accept_verify)
accept_call("agent-plan", "claim", "--id", "accept-task", "--provider", "codex")
accept_task = json.loads(accept_call("agent-plan", "evidence-snapshot", "--id", "accept-task").stdout)
accept_call("agent-plan", "start", "--id", "accept-task", "--lease-id", accept_task["lease_id"])
accept_execution = Path(accept_call("scratch-worktree", "add", "--owner-pid", str(os.getpid())).stdout.strip())
# The scratch inherits author config through the existing shared Git metadata.
accept_worker = accept_call("agent-events", "start", "--provider", "local", "--tool", "peer-delegate",
    "--task-id", "accept-task", "--parent-attempt-id", accept_env["OMS_PANEL_MAIN_ATTEMPT"],
    "--ref", "panel_role=worker", "--ref", "panel_access=write", "--ref", "panel_lease_id=" + accept_task["lease_id"],
    "--then", "starting", "--then", "working", "--then", "verifying", "--then", "review").stdout.strip()
accept_artifact = accept_repo / ".oms/artifacts/fixture/accept.md"
accept_artifact.parent.mkdir(parents=True, exist_ok=True)
accept_artifact.write_text("## Output\nReviewed fixture.\n## Exit\n0\n")
accept_patch = accept_artifact.with_suffix(".patch")
(accept_repo / "value.txt").write_text("old\naccepted\n")
(accept_repo / "literal*.txt").write_text("old\naccepted\n")
subprocess.run(["git", "--literal-pathspecs", "-C", str(accept_repo), "add", "-N", "--", "literal*.txt"],
               check=True)
accept_patch.write_bytes(subprocess.check_output(["git", "-C", str(accept_repo), "diff", "--binary"]))
(accept_repo / "value.txt").write_text("old\n")
(accept_repo / "literal*.txt").unlink()
subprocess.run(["git", "--literal-pathspecs", "-C", str(accept_repo), "reset", "--", "literal*.txt"],
               check=True, stdout=subprocess.DEVNULL)
accept_digest = hashlib.sha256(accept_patch.read_bytes()).hexdigest()
accept_index_env = dict(accept_env, OMS_TASK_ID="accept-task", OMS_ATTEMPT_ID=accept_worker,
                        OMS_INDEX_PLAN_ID=accept_task["plan_id"])
call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" delegate local 0 "$3" "$4"',
      "accept-fixture", str(root / "scripts/lib/peer-common.sh"), str(accept_repo),
      str(accept_artifact), str(accept_patch)], accept_index_env)
accept_review = json.dumps({"gate_verify_exit": 0, "candidate": {"patch_sha256": accept_digest},
                            "seats": [{"verdict": "pass"}]})
call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" review-outcome local 0 "$3"',
      "accept-fixture", str(root / "scripts/lib/peer-common.sh"), str(accept_repo), str(accept_artifact)],
     dict(accept_index_env, OMS_INDEX_REVIEW_OUTCOME_JSON=accept_review))
accept_options = ("--accept", "accept-task", "--message", "fix: accept reviewed fixture", "--verify", accept_verify,
                  "--expected-patch-sha256", accept_digest)
accept_base = subprocess.check_output(["git", "-C", str(accept_execution), "rev-parse", "HEAD"]).strip()
for refusal in ("digest", "dirty", "review", "lease", "ambiguous"):
    with patch.dict(os.environ, accept_env, clear=True):
        if refusal == "digest":
            original = accept_patch.read_bytes()
            accept_patch.write_bytes(original + b"\n")
        elif refusal == "dirty":
            (accept_execution / "unrelated.txt").write_text("preserve\n")
        try:
            if refusal in ("review", "lease", "ambiguous"):
                original_records = saved._records
                def rejected_records(*args, **kwargs):
                    attempts, records = deepcopy(original_records(*args, **kwargs))
                    if refusal == "review":
                        records = [r for r in records if r.get("kind") != "review-outcome"]
                    elif refusal == "lease":
                        next(a for a in attempts if a["attempt_id"] == accept_worker)["refs"]["panel_lease_id"] = "lease_" + "0" * 32
                    else:
                        duplicate = deepcopy(next(a for a in attempts if a["attempt_id"] == accept_worker))
                        duplicate["attempt_id"] = "att_" + "f" * 32
                        attempts.append(duplicate)
                    return attempts, records
                with patch.object(saved, "_records", side_effect=rejected_records):
                    try:
                        saved.accept(accept_repo, "codex", "accept-task", "fix: refused", accept_verify,
                                     accept_digest, accept_execution)
                        raise AssertionError(refusal + " accepted")
                    except ValueError:
                        pass
            else:
                rejected = accept_call("panel", *accept_options, execution=accept_execution, check=False)
                assert rejected.returncode != 0 and ("digest" if refusal == "digest" else "dirty") in rejected.stderr, rejected.stderr
            assert subprocess.check_output(["git", "-C", str(accept_execution), "rev-parse", "HEAD"]).strip() == accept_base
            assert (accept_execution / "value.txt").read_text() == "old\n"
        finally:
            if refusal == "digest":
                accept_patch.write_bytes(original)
            elif refusal == "dirty":
                (accept_execution / "unrelated.txt").unlink()

def prepare_accept_clone(label):
    clone = temporary / label
    subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(accept_repo), str(clone)], check=True)
    shutil.copytree(accept_repo / ".oms", clone / ".oms")
    for key, value in (("user.name", "accept fixture"), ("user.email", "accept@example.com")):
        subprocess.run(["git", "-C", str(clone), "config", key, value], check=True)
    return clone, dict(accept_env, OMS_ROOM_REPO=str(clone))

# A reviewed patch that changes its own verifier needs explicit owner consent.
consent_repo, consent_env = prepare_accept_clone("accept verifier consent")
consent_verify = "awk -f value.txt /dev/null"
accept_call("agent-plan", "add", "--id", "consent-task", "--title", "Consent fixture",
            "--allowed", "value.txt,literal[*].txt", "--verify", consent_verify,
            execution=consent_repo, env=consent_env)
accept_call("agent-plan", "claim", "--id", "consent-task", "--provider", "codex",
            execution=consent_repo, env=consent_env)
consent_task = json.loads(accept_call("agent-plan", "evidence-snapshot", "--id", "consent-task",
                                      execution=consent_repo, env=consent_env).stdout)
accept_call("agent-plan", "start", "--id", "consent-task", "--lease-id", consent_task["lease_id"],
            execution=consent_repo, env=consent_env)
consent_worker = accept_call("agent-events", "start", "--provider", "local", "--tool", "peer-delegate",
    "--task-id", "consent-task", "--parent-attempt-id", consent_env["OMS_PANEL_MAIN_ATTEMPT"],
    "--ref", "panel_role=worker", "--ref", "panel_access=write", "--ref", "panel_lease_id=" + consent_task["lease_id"],
    "--then", "starting", "--then", "working", "--then", "verifying", "--then", "review",
    execution=consent_repo, env=consent_env).stdout.strip()
consent_artifact = consent_repo / ".oms/artifacts/fixture/consent.md"
consent_artifact.write_text(accept_artifact.read_text())
consent_patch = consent_artifact.with_suffix(".patch")
consent_patch.write_bytes(accept_patch.read_bytes())
consent_index_env = dict(consent_env, OMS_TASK_ID="consent-task", OMS_ATTEMPT_ID=consent_worker,
                         OMS_INDEX_PLAN_ID=consent_task["plan_id"])
call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" delegate local 0 "$3" "$4"',
      "consent-fixture", str(root / "scripts/lib/peer-common.sh"), str(consent_repo),
      str(consent_artifact), str(consent_patch)], consent_index_env)
call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" review-outcome local 0 "$3"',
      "consent-fixture", str(root / "scripts/lib/peer-common.sh"), str(consent_repo), str(consent_artifact)],
     dict(consent_index_env, OMS_INDEX_REVIEW_OUTCOME_JSON=accept_review))
consent_options = ("--accept", "consent-task", "--message", "fix: consent fixture", "--verify", consent_verify)
consent_base = subprocess.check_output(["git", "-C", str(consent_repo), "rev-parse", "HEAD"])
refused = accept_call("panel", *consent_options, execution=consent_repo, env=consent_env, check=False)
assert refused.returncode != 0 and "REJECT" in refused.stderr, refused.stderr
assert any("patch modifies its own verifier" in p.read_text()
           for p in (consent_repo / ".oms/artifacts/admit").glob("*.md"))
assert subprocess.check_output(["git", "-C", str(consent_repo), "rev-parse", "HEAD"]) == consent_base
assert not subprocess.check_output(["git", "-C", str(consent_repo), "status", "--porcelain"])
original_run = subprocess.run
captured_admission = []
def inspect_consent_command(command, *args, **kwargs):
    if len(command) > 1 and str(command[1]).endswith("scripts/patch-land.sh"):
        captured_admission.append(command)
        return subprocess.CompletedProcess(command, 1)
    return original_run(command, *args, **kwargs)
with patch.dict(os.environ, consent_env, clear=True), patch.object(saved.subprocess, "run", side_effect=inspect_consent_command):
    try:
        saved.accept(consent_repo, "codex", "consent-task", "fix: consent fixture", consent_verify,
                     execution_repo=consent_repo, allow_verifier_change=True, allow_floor_change="0")
        raise AssertionError("mock admission unexpectedly succeeded")
    except ValueError as error:
        assert "patch-land failed" in str(error), error
assert len(captured_admission) == 1
assert captured_admission[0][-3:] == ["--allow-verifier-change", "--allow-floor-change", "0"]
consented = accept_call("panel", *consent_options, "--allow-verifier-change",
                        execution=consent_repo, env=consent_env)
assert "committed locally" in consented.stdout
consent_result = json.loads(accept_call("agent-plan", "show", "--id", "consent-task",
                                        execution=consent_repo, env=consent_env).stdout)
assert consent_result["state"] == "review" and consent_result["panel_accept"]["patch_sha256"] == accept_digest
assert "allow_verifier_change" not in consent_result["panel_accept"]
assert "allow_floor_change" not in consent_result["panel_accept"]
assert not subprocess.check_output(["git", "-C", str(consent_repo), "status", "--porcelain"])
for consent_flag in (("--allow-verifier-change",), ("--allow-floor-change", "reason")):
    invalid = accept_call("panel", *consent_flag, execution=consent_repo, env=consent_env, check=False)
    assert invalid.returncode == 2 and "require --accept" in invalid.stderr, invalid.stderr
for reason in ("", " ", "\n", "bad\rvalue", "bad\tvalue", "bad\x7fvalue"):
    invalid = accept_call("panel", *consent_options, "--allow-floor-change", reason,
                          execution=consent_repo, env=consent_env, check=False)
    assert invalid.returncode == 2 and "--allow-floor-change" in invalid.stderr, invalid.stderr

# A commit hook that mutates the index must never run for the internal exact-tree
# accept commit.
hook_repo, hook_env = prepare_accept_clone("accept commit hook")
hook_path = hook_repo / ".git/hooks/pre-commit"
hook_path.parent.mkdir(parents=True, exist_ok=True)
hook_path.write_text("#!/bin/sh\nprintf 'unreviewed\\n' > hook-added.txt\ngit add -- hook-added.txt\n", encoding="utf-8")
hook_path.chmod(0o755)
hook_base = subprocess.check_output(["git", "-C", str(hook_repo), "rev-parse", "HEAD"], text=True).strip()
hook_index = temporary / "accept-hook-index"
hook_index_env = dict(os.environ, GIT_INDEX_FILE=str(hook_index), GIT_CONFIG_GLOBAL="/dev/null",
                      GIT_CONFIG_SYSTEM="/dev/null")
subprocess.run(["git", "-C", str(hook_repo), "read-tree", hook_base], env=hook_index_env, check=True)
subprocess.run(["git", "-C", str(hook_repo), "apply", "--cached", "--binary", str(accept_patch)],
               env=hook_index_env, check=True)
hook_expected_tree = subprocess.check_output(["git", "-C", str(hook_repo), "write-tree"],
                                              env=hook_index_env, text=True).strip()
hook_env.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null")
with patch.dict(os.environ, hook_env, clear=True):
    hook_accept = saved.accept(hook_repo, "codex", "accept-task", "fix: exact reviewed tree",
                               accept_verify, accept_digest, hook_repo)
assert subprocess.check_output(["git", "-C", str(hook_repo), "rev-parse", "HEAD^{tree}"],
                               text=True).strip() == hook_expected_tree
assert not (hook_repo / "hook-added.txt").exists()
assert not subprocess.check_output(["git", "-C", str(hook_repo), "status", "--porcelain"])
assert hook_accept["sha"] == subprocess.check_output(["git", "-C", str(hook_repo), "rev-parse", "HEAD"],
                                                      text=True).strip()

# Successive ordinary accepts must preserve the already accepted verifier,
# even without a goal batch or a finalized/published first task.
prefix_repo, prefix_env = prepare_accept_clone("accept cumulative scratch")
prefix_execution = Path(accept_call("scratch-worktree", "add", "--owner-pid", str(os.getpid()),
                                     execution=prefix_repo, env=prefix_env).stdout.strip())
accept_call("panel", *accept_options, execution=prefix_execution, env=prefix_env)
prefix_first_sha = subprocess.check_output(["git", "-C", str(prefix_execution), "rev-parse", "HEAD"],
                                          text=True).strip()
# The clone's accept leaves one content-free panel-accept row in the canonical index the board reads.
prefix_accepts = [json.loads(line) for line in (prefix_repo / ".oms/artifacts/index.jsonl").read_text().splitlines()
                  if '"panel-accept"' in line]
assert len(prefix_accepts) == 1 and prefix_accepts[0]["task_id"] == "accept-task", prefix_accepts
assert prefix_accepts[0]["patch_sha256"] == accept_digest and prefix_first_sha.startswith(prefix_accepts[0]["base_sha"]), prefix_accepts
assert prefix_accepts[0].get("attempt_id") and prefix_accepts[0].get("exit") == 0, prefix_accepts
for sibling, contents in (("breaks-first", "broken\n"), ("independent", "old\naccepted\n")):
    sibling_verify = "test -f sibling.txt"
    accept_call("agent-plan", "add", "--id", sibling, "--title", sibling,
                "--allowed", "value.txt,sibling.txt", "--verify", sibling_verify,
                execution=prefix_repo, env=prefix_env)
    accept_call("agent-plan", "claim", "--id", sibling, "--provider", "codex", execution=prefix_repo, env=prefix_env)
    sibling_task = json.loads(accept_call("agent-plan", "evidence-snapshot", "--id", sibling,
                                         execution=prefix_repo, env=prefix_env).stdout)
    accept_call("agent-plan", "start", "--id", sibling, "--lease-id", sibling_task["lease_id"],
                execution=prefix_repo, env=prefix_env)
    sibling_worker = accept_call("agent-events", "start", "--provider", "local", "--tool", "peer-delegate",
        "--task-id", sibling, "--parent-attempt-id", prefix_env["OMS_PANEL_MAIN_ATTEMPT"],
        "--ref", "panel_role=worker", "--ref", "panel_access=write",
        "--ref", "panel_lease_id=" + sibling_task["lease_id"],
        "--then", "starting", "--then", "working", "--then", "verifying", "--then", "review",
        execution=prefix_repo, env=prefix_env).stdout.strip()
    sibling_artifact = prefix_repo / (".oms/artifacts/fixture/" + sibling + ".md")
    sibling_artifact.write_text("## Output\nReviewed sibling.\n## Exit\n0\n")
    sibling_patch = sibling_artifact.with_suffix(".patch")
    (prefix_execution / "value.txt").write_text(contents)
    (prefix_execution / "sibling.txt").write_text("independent\n")
    subprocess.run(["git", "-C", str(prefix_execution), "add", "-N", "sibling.txt"], check=True)
    sibling_patch.write_bytes(subprocess.check_output(["git", "-C", str(prefix_execution), "diff", "--binary"]))
    (prefix_execution / "value.txt").write_text("old\naccepted\n")
    (prefix_execution / "sibling.txt").unlink()
    subprocess.run(["git", "-C", str(prefix_execution), "reset", "--", "sibling.txt"], check=True,
                   stdout=subprocess.DEVNULL)
    sibling_digest = hashlib.sha256(sibling_patch.read_bytes()).hexdigest()
    sibling_env = dict(prefix_env, OMS_TASK_ID=sibling, OMS_ATTEMPT_ID=sibling_worker,
                       OMS_INDEX_PLAN_ID=sibling_task["plan_id"])
    call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" delegate local 0 "$3" "$4"',
          "sibling-fixture", str(root / "scripts/lib/peer-common.sh"), str(prefix_repo),
          str(sibling_artifact), str(sibling_patch)], sibling_env)
    sibling_review = json.dumps({"gate_verify_exit": 0, "candidate": {"patch_sha256": sibling_digest},
                                 "seats": [{"verdict": "pass"}]})
    call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" review-outcome local 0 "$3"',
          "sibling-fixture", str(root / "scripts/lib/peer-common.sh"), str(prefix_repo), str(sibling_artifact)],
         dict(sibling_env, OMS_INDEX_REVIEW_OUTCOME_JSON=sibling_review))
    sibling_result = accept_call("panel", "--accept", sibling, "--message", "fix: " + sibling,
                                 "--verify", sibling_verify, execution=prefix_execution, env=prefix_env, check=False)
    if sibling == "breaks-first":
        assert sibling_result.returncode != 0, "ordinary sibling acceptance broke accept-task without replay"
        assert "accept-task" in sibling_result.stderr, sibling_result.stderr
        assert subprocess.check_output(["git", "-C", str(prefix_execution), "rev-parse", "HEAD"],
                                       text=True).strip() == prefix_first_sha
        assert not subprocess.check_output(["git", "-C", str(prefix_execution), "status", "--porcelain"])
    else:
        assert sibling_result.returncode == 0, sibling_result.stderr
        reports = list((prefix_repo / ".oms/artifacts/admit").glob("*.md"))
        assert any("- verify-prefix-accept-task: PASS" in report.read_text()
                   for report in reports), "accepted sibling did not replay A"

import runpy
prefix_capture = runpy.run_path(str(root / "scripts/lib/landing-capture.py"))
prefix_raw_landings = prefix_capture["rows"](prefix_repo)
prefix_landings = prefix_capture["panel_accept_intents"](prefix_raw_landings)
round2_audit_failures = []
with patch.dict(os.environ, prefix_env, clear=True):
    prefix_context = prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first")
    assert [item["id"] for item in prefix_context["checks"]] == ["accept-task", "independent"]
    accepted_intent = next(row for row in prefix_raw_landings
                           if row.get("event") == "intent" and row.get("task") == "accept-task")
    abandoned_intent = dict(accepted_intent, landing_id="land-prefix-abandoned", event="intent")
    receipt_fields = ("landing_id", "patch", "patch_sha", "base_sha", "task", "lease",
                      "plan_receipt_sha", "plan_done_receipt_sha", "approval", "approval_version")
    receipt_identity = dict(schema=1, **{name: abandoned_intent.get(name, "") for name in receipt_fields})
    abandoned_sha = hashlib.sha256(json.dumps(receipt_identity, ensure_ascii=False, sort_keys=True,
        separators=(",", ":")).encode()).hexdigest()
    abandoned_intent["receipt_sha"] = abandoned_sha
    abandoned_terminal = dict(abandoned_intent, event="abandoned")
    abandoned_log = prefix_raw_landings + [abandoned_intent, abandoned_terminal]
    projected_abandoned = prefix_capture["panel_accept_intents"](abandoned_log)
    assert next(row for row in projected_abandoned if row["landing_id"] == "land-prefix-abandoned")[
        "_has_abandoned"] is True
    with patch.dict(prefix_capture["batch_prefix"].__globals__, {"rows": lambda _repo: abandoned_log}):
        abandoned_context = prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first")
    assert abandoned_context["checks"] == prefix_context["checks"]
    with patch.dict(os.environ, OMS_HARNESS_CHILD="1", OMS_HARNESS_DELEGATE_DEPTH="0"), \
            patch.object(saved, "_replay_patch_tree", side_effect=AssertionError("Git tree replay ran in child")) as replay:
        assert prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "") == {"batches": [], "checks": []}
        try:
            prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first")
        except ValueError as error:
            if "parent-only" not in str(error):
                round2_audit_failures.append("child derivation was not refused at the parent-only boundary: %s" % error)
        else:
            round2_audit_failures.append("panel prefix derivation ran in a harness child")
        assert not replay.called
        with patch.object(saved, "_execution_repo", side_effect=lambda repo, execution: Path(execution)), \
                patch.object(saved, "_replay_patch_tree",
                             side_effect=AssertionError("Git tree replay ran in child")) as replay:
            try:
                saved._accepted_prefix(prefix_repo, prefix_execution, prefix_landings)
            except ValueError as error:
                if "parent-only" not in str(error):
                    round2_audit_failures.append("direct child prefix derivation lacked a parent-only refusal: %s" % error)
            except AssertionError as error:
                round2_audit_failures.append("direct child prefix derivation called the tree writer: %s" % error)
            else:
                round2_audit_failures.append("direct panel prefix derivation ran in a harness child")
            if replay.called:
                round2_audit_failures.append("direct child prefix derivation called the tree writer")
    # Shared canonical evidence in another clone must not supply replay commands.
    foreign_landings = [dict(row, worktree=str(prefix_repo)) for row in prefix_landings]
    assert saved._accepted_prefix(prefix_repo, prefix_execution, foreign_landings) == []
    prefix_read = saved._read
    def missing_accept_record(repo, relative, *args, **kwargs):
        raw = prefix_read(repo, relative, *args, **kwargs)
        if relative == ".oms/plan/tasks.json":
            value = json.loads(raw)
            value["tasks"]["accept-task"].pop("panel_accept")
            return json.dumps(value).encode()
        return raw
    with patch.object(saved, "_read", side_effect=missing_accept_record):
        try:
            saved._accepted_prefix(prefix_repo, prefix_execution, prefix_landings)
        except ValueError as error:
            if "accept-task" not in str(error):
                round2_audit_failures.append("missing panel_accept record did not name its task: %s" % error)
        else:
            round2_audit_failures.append("journaled acceptance without its task record was ignored")
    def unreadable_accept_record(repo, relative, *args, **kwargs):
        if relative == ".oms/plan/tasks.json":
            raise ValueError("fixture task record is unreadable")
        return prefix_read(repo, relative, *args, **kwargs)
    with patch.object(saved, "_read", side_effect=unreadable_accept_record):
        try:
            prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first")
        except ValueError as error:
            if "accept-task" not in str(error):
                round2_audit_failures.append("unreadable panel_accept record did not name its task: %s" % error)
        else:
            round2_audit_failures.append("unreadable panel_accept record was ignored")
    with patch.dict(os.environ, prefix_env, clear=True), \
            patch.object(saved, "_read", side_effect=unreadable_accept_record):
        try:
            saved._owned_task(prefix_repo, prefix_execution, "codex", "accept-task")
        except ValueError as error:
            if "accept-task" not in str(error):
                round2_audit_failures.append("unreadable owner task record did not name its task: %s" % error)
        else:
            round2_audit_failures.append("unreadable owner task record was ignored")
    accept_patch_path = json.loads(prefix_read(prefix_repo, ".oms/plan/tasks.json"))["tasks"][
        "accept-task"]["panel_accept"]["patch"]
    def unreadable_accept_patch(repo, relative, *args, **kwargs):
        if relative == accept_patch_path:
            raise ValueError("fixture patch evidence is missing")
        return prefix_read(repo, relative, *args, **kwargs)
    with patch.object(saved, "_read", side_effect=unreadable_accept_patch):
        try:
            saved._accepted_prefix(prefix_repo, prefix_execution, prefix_landings)
        except ValueError as error:
            if "accept-task" not in str(error):
                round2_audit_failures.append("missing patch evidence did not name its task: %s" % error)
        else:
            round2_audit_failures.append("unreadable accepted patch evidence was ignored")
    def changed_prefix_record(repo, relative, *args, **kwargs):
        raw = prefix_read(repo, relative, *args, **kwargs)
        if relative == ".oms/plan/tasks.json":
            value = json.loads(raw)
            value["tasks"]["accept-task"]["panel_accept"]["base"] = "0" * 40
            return json.dumps(value).encode()
        return raw
    with patch.object(saved, "_read", side_effect=changed_prefix_record):
        try:
            saved._accepted_prefix(prefix_repo, prefix_execution, prefix_landings)
            raise AssertionError("changed canonical acceptance lineage was ignored")
        except ValueError as error:
            assert "lineage changed: accept-task" in str(error), error
    prefix_parameters = ("test -f sibling.txt", "0", "breaks-first", "0", "0", "", "0", "0")
    prefix_binding = prefix_capture["admission_binding"](prefix_repo, prefix_execution, *prefix_parameters)
    prefix_receipt_dir = Path(call(["bash", "-c", '. "$1"; oms_land_state_dir "$2"', "prefix-receipt",
        str(root / "scripts/lib/land-receipts.sh"), str(prefix_execution)], prefix_env).strip())
    prefix_receipt_dir.mkdir(parents=True, exist_ok=True)
    prefix_head = subprocess.check_output(["git", "-C", str(prefix_execution), "rev-parse", "HEAD"],
                                          text=True).strip()
    prefix_receipt = {"schema": 1, "state": "passed", "sha": prefix_head,
                      "gate": {"rc": 0}, "push": {"rc": 0, "moved": 1}, "ci": {"conclusion": "success"}}
    prefix_receipt_file = prefix_receipt_dir / (prefix_head + "-fixture.json")
    prefix_receipt_file.write_text(json.dumps(dict(prefix_receipt, state="failed")))
    assert prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first") == prefix_context
    skipped_ci_receipt = dict(prefix_receipt, ci={"conclusion": "skipped"})
    prefix_receipt_file.write_text(json.dumps(skipped_ci_receipt))
    assert prefix_capture["batch_prefix"](prefix_repo, prefix_execution, "breaks-first")["checks"] == []
    reset_binding = prefix_capture["admission_binding"](prefix_repo, prefix_execution, *prefix_parameters)
    assert reset_binding["batch_prefix_sha256"] != prefix_binding["batch_prefix_sha256"]
assert not round2_audit_failures, round2_audit_failures

# A lifecycle heartbeat during patch-land admission changes the worker's
# activity fields but must not invalidate the reviewed authority.
heartbeat_repo, heartbeat_env = prepare_accept_clone("accept heartbeat race")
heartbeat_attempts = saved._records(heartbeat_repo)[0]
heartbeat_before = next(a for a in heartbeat_attempts if a["attempt_id"] == accept_worker)
accept_admission_hook.write_text(
    "#!/bin/sh\nset -eu\nbash %s agent-events --repo %s heartbeat --attempt %s\n" % (
        shlex.quote(str(panel.ENTRY)), shlex.quote(str(heartbeat_repo)), shlex.quote(accept_worker)),
    encoding="utf-8")
try:
    with patch.dict(os.environ, heartbeat_env, clear=True):
        admission_heartbeat_accept = saved.accept(heartbeat_repo, "codex", "accept-task",
            "fix: admission heartbeat race", accept_verify, accept_digest, heartbeat_repo)
finally:
    accept_admission_hook.unlink(missing_ok=True)
assert admission_heartbeat_accept["sha"]
admission_heartbeat_after = next(a for a in saved._records(heartbeat_repo)[0]
                                 if a["attempt_id"] == accept_worker)
assert admission_heartbeat_after.get("last_heartbeat_at") != heartbeat_before.get("last_heartbeat_at")
assert (heartbeat_repo / "value.txt").read_text() == "old\naccepted\n"
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=heartbeat_repo,
                              env=heartbeat_env).stdout)["state"] == "review"

# Keep the post-application heartbeat race too: it exercises the same stable
# authority comparison in panel_results' follow-up snapshot.
heartbeat_repo, heartbeat_env = prepare_accept_clone("accept post-apply heartbeat race")
heartbeat_attempts = saved._records(heartbeat_repo)[0]
heartbeat_before = next(a for a in heartbeat_attempts if a["attempt_id"] == accept_worker)
original_preflight = saved._accept_preflight
preflight_calls = [0]
def append_heartbeat_after_apply(*args, **kwargs):
    preflight_calls[0] += 1
    if preflight_calls[0] == 2:
        heartbeat = accept_call("agent-events", "heartbeat", "--attempt", accept_worker,
                                execution=heartbeat_repo, env=heartbeat_env)
        assert heartbeat.stdout.strip()
    return original_preflight(*args, **kwargs)
with patch.dict(os.environ, heartbeat_env, clear=True), \
        patch.object(saved, "_accept_preflight", side_effect=append_heartbeat_after_apply):
    heartbeat_accept = saved.accept(heartbeat_repo, "codex", "accept-task", "fix: heartbeat race",
                                    accept_verify, accept_digest, heartbeat_repo)
assert heartbeat_accept["sha"]
heartbeat_after = next(a for a in saved._records(heartbeat_repo)[0] if a["attempt_id"] == accept_worker)
assert heartbeat_after.get("last_heartbeat_at") != heartbeat_before.get("last_heartbeat_at")
assert (heartbeat_repo / "value.txt").read_text() == "old\naccepted\n"
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=heartbeat_repo,
                              env=heartbeat_env).stdout)["state"] == "review"

# A passing review-gate replacement at that same pre-apply boundary is a real
# authority change and must leave the integration tree untouched.
authority_admission_repo, authority_admission_env = prepare_accept_clone("accept admission authority race")
authority_admission_base = subprocess.check_output(
    ["git", "-C", str(authority_admission_repo), "rev-parse", "HEAD"]).strip()
with patch.dict(os.environ, authority_admission_env, clear=True):
    authority_admission_before = saved._accept_preflight(
        authority_admission_repo, authority_admission_repo, "codex", "accept-task", accept_verify, accept_digest)
review_hook = '. %s; ma_append_artifact_index %s review-outcome local 0 %s\n' % (
    shlex.quote(str(root / "scripts/lib/peer-common.sh")),
    shlex.quote(str(authority_admission_repo)),
    shlex.quote(str(authority_admission_repo / ".oms/artifacts/fixture/accept.md")))
for name, value in (("OMS_TASK_ID", "accept-task"), ("OMS_ATTEMPT_ID", accept_worker),
                    ("OMS_INDEX_PLAN_ID", accept_task["plan_id"]),
                    ("OMS_INDEX_REVIEW_OUTCOME_JSON", accept_review)):
    review_hook = "export %s=%s\n" % (name, shlex.quote(value)) + review_hook
accept_admission_hook.write_text("#!/bin/sh\nset -eu\n" + review_hook, encoding="utf-8")
try:
    with patch.dict(os.environ, authority_admission_env, clear=True):
        try:
            saved.accept(authority_admission_repo, "codex", "accept-task", "fix: admission authority race",
                         accept_verify, accept_digest, authority_admission_repo)
            raise AssertionError("changed review gate during admission was accepted")
        except ValueError as error:
            assert "patch-land failed" in str(error), error
finally:
    accept_admission_hook.unlink(missing_ok=True)
with patch.dict(os.environ, authority_admission_env, clear=True):
    authority_admission_after = saved._accept_preflight(
        authority_admission_repo, authority_admission_repo, "codex", "accept-task", accept_verify, accept_digest)
assert saved._accept_authority(authority_admission_after) != saved._accept_authority(authority_admission_before)
assert subprocess.check_output(["git", "-C", str(authority_admission_repo), "rev-parse", "HEAD"]).strip() == authority_admission_base
assert (authority_admission_repo / "value.txt").read_text() == "old\n"
assert not subprocess.check_output(["git", "-C", str(authority_admission_repo),
                                    "status", "--porcelain", "--untracked-files=all"])
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=authority_admission_repo,
                              env=authority_admission_env).stdout)["state"] == "running"

# A changed passing gate is still an authority change. Stop before commit and
# print exact commands for preserving the already-applied bytes and recording review.
authority_repo, authority_env = prepare_accept_clone("accept authority race")
original_preflight = saved._accept_preflight
preflight_calls = [0]
def append_replacement_review_after_apply(*args, **kwargs):
    preflight_calls[0] += 1
    if preflight_calls[0] == 2:
        review_env = dict(authority_env, OMS_TASK_ID="accept-task", OMS_ATTEMPT_ID=accept_worker,
                          OMS_INDEX_PLAN_ID=accept_task["plan_id"], OMS_INDEX_REVIEW_OUTCOME_JSON=accept_review)
        call(["bash", "-c", '. "$1"; ma_append_artifact_index "$2" review-outcome local 0 "$3"',
              "accept-review-race", str(root / "scripts/lib/peer-common.sh"), str(authority_repo),
              str(authority_repo / ".oms/artifacts/fixture/accept.md")], review_env)
    return original_preflight(*args, **kwargs)
authority_base = subprocess.check_output(["git", "-C", str(authority_repo), "rev-parse", "HEAD"]).strip()
with patch.dict(os.environ, authority_env, clear=True), \
        patch.object(saved, "_accept_preflight", side_effect=append_replacement_review_after_apply):
    try:
        saved.accept(authority_repo, "codex", "accept-task", "fix: authority race", accept_verify,
                     accept_digest, authority_repo)
        raise AssertionError("changed review gate was accepted")
    except ValueError as error:
        recovery = str(error)
        assert "authority changed after application" in recovery, recovery
        assert accept_digest in recovery, recovery
        assert "git --literal-pathspecs -C" in recovery and "add --" in recovery, recovery
        assert "_accept_recover" in recovery and "oms agent-plan --repo" in recovery, recovery
        assert "review --id accept-task" in recovery, recovery
        assert '"task_id":"accept-task"' in recovery, recovery
        assert '"lease_id":"' + accept_task["lease_id"] + '"' in recovery, recovery
        assert '"artifact":".oms/artifacts/fixture/accept.md"' in recovery, recovery
        assert '"patch":".oms/artifacts/fixture/accept.patch"' in recovery, recovery
assert subprocess.check_output(["git", "-C", str(authority_repo), "rev-parse", "HEAD"]).strip() == authority_base
assert (authority_repo / "value.txt").read_text() == "old\naccepted\n"
authority_status = subprocess.check_output(["git", "-C", str(authority_repo), "status", "--porcelain"])
assert b" M value.txt" in authority_status and b"?? literal*.txt" in authority_status
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=authority_repo,
                              env=authority_env).stdout)["state"] == "running"
recovery_commands = recovery.split("Recovery after confirming the reviewed bytes are still present; do not reapply:\n", 1)[1].splitlines()
assert len(recovery_commands) == 2 and recovery_commands[0].startswith("git --literal-pathspecs -C "), recovery_commands
(authority_repo / "literalA.txt").write_text("unrelated\n")
subprocess.run(["bash", "-c", recovery_commands[0]], env=authority_env, check=True)
staged_paths = subprocess.check_output(["git", "-C", str(authority_repo), "diff", "--cached", "--name-only"],
                                       text=True).splitlines()
assert staged_paths == ["literal*.txt", "value.txt"], staged_paths
assert (authority_repo / "literalA.txt").exists()
(authority_repo / "literalA.txt").unlink()
subprocess.run(["bash", "-c", recovery_commands[1]], env=authority_env, check=True)
authority_recovered = json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=authority_repo,
                                             env=authority_env).stdout)
assert authority_recovered["state"] == "review" and authority_recovered["panel_accept"]["patch_sha256"] == accept_digest
assert not subprocess.check_output(["git", "-C", str(authority_repo), "status", "--porcelain"])

# Partial failures preserve the application/index or the new commit. Each uses
# an independent clone and copied fixture evidence, never the owner's repository.
for failed_stage in ("commit", "plan review"):
    failure_repo = temporary / ("accept failure " + failed_stage)
    subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(accept_repo), str(failure_repo)], check=True)
    shutil.copytree(accept_repo / ".oms", failure_repo / ".oms")
    for key, value in (("user.name", "accept fixture"), ("user.email", "accept@example.com")):
        subprocess.run(["git", "-C", str(failure_repo), "config", key, value], check=True)
    failure_env = dict(accept_env, OMS_ROOM_REPO=str(failure_repo))
    original_exact, original_plan = saved._exact_commit_tree, saved._plan_command
    def failing_exact(execution, base, tree, message, authority_check=None):
        if failed_stage == "commit":
            raise ValueError("fixture commit failure")
        return original_exact(execution, base, tree, message, authority_check)
    def failing_plan(execution, *args, **kwargs):
        if args[0] == "review" and failed_stage == "plan review":
            raise ValueError("fixture review failure")
        return original_plan(execution, *args, **kwargs)
    with patch.dict(os.environ, failure_env, clear=True), patch.object(saved, "_exact_commit_tree", side_effect=failing_exact), \
            patch.object(saved, "_plan_command", side_effect=failing_plan):
        try:
            saved.accept(failure_repo, "codex", "accept-task", "fix: partial failure", accept_verify, accept_digest)
            raise AssertionError("partial failure unexpectedly succeeded")
        except ValueError as error:
            failure_message = str(error)
            failure_sha = subprocess.check_output(["git", "-C", str(failure_repo), "rev-parse", "HEAD"], text=True).strip()
            assert failed_stage + " failed" in str(error) and failure_sha in str(error), error
    assert (failure_repo / "value.txt").read_text() == "old\naccepted\n"
    status = subprocess.check_output(["git", "-C", str(failure_repo), "status", "--porcelain"])
    if failed_stage == "commit":
        assert failure_sha.encode() == accept_base and b"M  value.txt" in status and b"A  literal*.txt" in status
    else:
        assert not status
    assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=failure_repo,
                                  env=failure_env).stdout)["state"] == "running"
    refused_retry = accept_call("panel", *accept_options, execution=failure_repo, env=failure_env, check=False)
    assert refused_retry.returncode != 0
    assert ("dirty" if failed_stage == "commit" else "already applied") in refused_retry.stderr, refused_retry.stderr
    if failed_stage == "plan review":
        recovery_commands = failure_message.split(
            "Recovery after confirming the reviewed bytes are still present; do not reapply:\n", 1)[1].splitlines()
        assert len(recovery_commands) == 1 and "_accept_recover" in recovery_commands[0]
        accept_rows = lambda: (failure_repo / ".oms/artifacts/index.jsonl").read_text().count('"panel-accept"')
        rows_before = accept_rows()
        subprocess.run(["bash", "-c", recovery_commands[0]], env=failure_env, check=True)
        assert accept_rows() == rows_before + 1, "recovered accept left no canonical panel-accept row"
        assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=failure_repo,
                                      env=failure_env).stdout)["state"] == "review"
accepted = accept_call("panel", *accept_options, execution=accept_execution)
assert "committed locally; publication pending" in accepted.stdout and "Next: oms land --repo" in accepted.stdout
assert "Then: oms panel --finalize --repo" in accepted.stdout and "--landed-commit <full head SHA" in accepted.stdout
accept_sha = subprocess.check_output(["git", "-C", str(accept_execution), "rev-parse", "HEAD"], text=True).strip()
assert accept_sha.encode() != accept_base
assert (accept_execution / "value.txt").read_text() == "old\naccepted\n"
assert (accept_repo / "value.txt").read_text() == "old\n", "accept edited canonical source instead of execution clone"
accepted_task = json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)
assert accepted_task["state"] == "review"
accept_record = accepted_task["panel_accept"]
assert accept_record["commit"] == accept_sha and accept_record["tree"] == \
    subprocess.check_output(["git", "-C", str(accept_execution), "rev-parse", "HEAD^{tree}"], text=True).strip()
assert accept_record["base"] == accept_base.decode() and accept_record["lease_id"] == accepted_task["lease_id"]
assert accept_record["patch_sha256"] == accept_digest and accept_record["task_id"] == "accept-task"
accept_landing_rows = [json.loads(line) for line in (accept_repo / ".oms/landings.jsonl").read_text().splitlines()]
accept_intent = next(row for row in accept_landing_rows if row.get("event") == "intent")
assert (accept_intent["task"], accept_intent["plan_id"], accept_intent["lease"]) == (
    "accept-task", accept_task["plan_id"], accept_task["lease_id"])
accept_call("patch-land", "--recover", execution=accept_execution)
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review", \
    "application recovery finished the plan before publication"
assert not subprocess.check_output(["git", "-C", str(accept_execution), "status", "--porcelain"])
retry_accept = accept_call("panel", *accept_options, execution=accept_execution, check=False)
assert retry_accept.returncode != 0 and "requires a claimed/running task" in retry_accept.stderr
# A SHA alone is insufficient; the real finish command must require publication proof.
accept_finalize = ("--finalize", "--task-id", "accept-task", "--owner", "codex", "--landed-commit", accept_sha,
                   "--summary-file", str(summary), "--outcome", "accepted", "--verify", accept_verify,
                   "--evidence", ".oms/artifacts/fixture/accept.patch")
for invalid_outcome in ("failed", "rejected"):
    invalid_finalize = list(accept_finalize)
    invalid_finalize[invalid_finalize.index("--outcome") + 1] = invalid_outcome
    invalid = accept_call("panel", *invalid_finalize, execution=accept_execution, check=False)
    assert invalid.returncode != 0 and \
        "--landed-commit requires --outcome accepted or completed" in invalid.stderr, invalid.stderr
    assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review"
with patch.dict(os.environ, accept_env, clear=True), patch.object(saved, "_plan_command") as plan_command:
    try:
        saved.finalize(accept_repo, "codex", "accept-task", str(summary), "rejected",
                       verify=accept_verify, evidence=".oms/artifacts/fixture/accept.patch",
                       execution_repo=accept_execution, landed_commit=accept_sha)
        raise AssertionError("API accepted --landed-commit with a rejected outcome")
    except ValueError as error:
        assert "--landed-commit requires --outcome accepted or completed" in str(error), error
    assert not plan_command.called, "invalid outcome ran plan finish"
bad_summary = list(accept_finalize)
bad_summary[bad_summary.index("--summary-file") + 1] = str(accept_repo / "missing-summary.txt")
pre_finish_error = accept_call("panel", *bad_summary, execution=accept_execution, check=False)
assert pre_finish_error.returncode != 0 and "validating completion" not in pre_finish_error.stderr, pre_finish_error.stderr
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review"
unpublished = accept_call("panel", *accept_finalize, execution=accept_execution, check=False)
assert unpublished.returncode != 0 and "finish failed" in unpublished.stderr
assert "retry finish only after validating completion" in unpublished.stderr
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review"
# Fixture-only publication evidence: no push, provider, network or real land.
receipt_dir = Path(call(["bash", "-c", '. "$1"; oms_land_state_dir "$2"', "accept-receipt",
                        str(root / "scripts/lib/land-receipts.sh"), str(accept_execution)], accept_env).strip())
receipt_dir.mkdir(parents=True, exist_ok=True)
# A real publication of an older, unrelated commit that edits the same file
# cannot finish the exact worker acceptance.
unrelated_index = temporary / "accept-unrelated-index"
unrelated_env = dict(os.environ, GIT_INDEX_FILE=str(unrelated_index))
subprocess.run(["git", "-C", str(accept_execution), "read-tree", accept_base.decode()],
               env=unrelated_env, check=True)
unrelated_blob = subprocess.check_output(["git", "-C", str(accept_execution), "hash-object", "-w", "--stdin"],
                                         input=b"old\nunrelated publication\n").decode().strip()
subprocess.run(["git", "-C", str(accept_execution), "update-index", "--add", "--cacheinfo",
                "100644," + unrelated_blob + ",value.txt"], env=unrelated_env, check=True)
unrelated_tree = subprocess.check_output(["git", "-C", str(accept_execution), "write-tree"],
                                        env=unrelated_env, text=True).strip()
unrelated_sha = subprocess.check_output(["git", "-C", str(accept_execution), "commit-tree", unrelated_tree,
                                         "-p", accept_base.decode(), "-m", "older unrelated publication"],
                                        text=True).strip()
unrelated_receipt = {"schema": 1, "state": "passed", "sha": unrelated_sha, "remote": "origin", "target": "main",
                     "gate": {"rc": 0, "command": "true"},
                     "push": {"rc": 0, "moved": 1, "base": accept_base.decode()},
                     "ci": {"conclusion": "success"}}
(receipt_dir / (unrelated_sha + "-fixture.json")).write_text(json.dumps(unrelated_receipt))
subprocess.run(["git", "-C", str(accept_execution), "update-ref", "refs/remotes/origin/main", unrelated_sha], check=True)
unrelated_finalize = list(accept_finalize)
unrelated_finalize[unrelated_finalize.index("--landed-commit") + 1] = unrelated_sha
unrelated = accept_call("panel", *unrelated_finalize, execution=accept_execution, check=False)
assert unrelated.returncode != 0 and "does not contain the accepted commit" in unrelated.stderr, unrelated.stderr
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review"
# Direct plan finish must enforce the same acceptance binding inside its locked
# transition; the panel wrapper is not the authority for this invariant.
direct_unrelated = accept_call("agent-plan", "finish", "--id", "accept-task", "--lease-id",
                               accepted_task["lease_id"], "--landed-commit", unrelated_sha,
                               execution=accept_execution, check=False)
assert direct_unrelated.returncode != 0 and "does not contain the accepted commit" in \
    direct_unrelated.stderr, direct_unrelated.stderr
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)["state"] == "review"
# A plain patch-land without panel acceptance must name the --accept repair path.
with patch.object(saved, "_owned_task", return_value={"plan_id": "plan_x", "lease_id": "l1"}):
    try:
        saved._validate_landed_acceptance(accept_repo, accept_execution, "codex", "accept-task", accept_verify,
                                          accept_sha)
        raise AssertionError("landed acceptance without panel_accept was accepted")
    except ValueError as error:
        assert "(it was not landed with oms panel --accept)" in str(error) and \
            "--plan-task" in str(error), error
# Stable patch IDs ignore whitespace. A different Python indentation must not
# bind the reviewed patch even though it locates the same candidate class.
indent_repo = temporary / "accept indentation collision"
subprocess.run(["git", "init", "-q", str(indent_repo)], check=True)
for key, value in (("user.name", "indent fixture"), ("user.email", "indent@example.com")):
    subprocess.run(["git", "-C", str(indent_repo), "config", key, value], check=True)
indent_sources = {
    "base": b"def sample():\n    return 1\n",
    "reviewed": b"def sample():\n        return 1\n",
    "published": b"def sample():\n      return 1\n",
}
indent_trees = {}
for label, source in indent_sources.items():
    index_path = temporary / ("accept-indent-" + label + ".index")
    index_env = dict(os.environ, GIT_INDEX_FILE=str(index_path))
    if label == "base":
        subprocess.run(["git", "-C", str(indent_repo), "read-tree", "--empty"], env=index_env, check=True)
    else:
        subprocess.run(["git", "-C", str(indent_repo), "read-tree", indent_trees["base"]],
                       env=index_env, check=True)
    blob = subprocess.check_output(["git", "-C", str(indent_repo), "hash-object", "-w", "--stdin"],
                                   input=source).decode().strip()
    subprocess.run(["git", "-C", str(indent_repo), "update-index", "--add", "--cacheinfo",
                    "100644," + blob + ",format.py"], env=index_env, check=True)
    indent_trees[label] = subprocess.check_output(["git", "-C", str(indent_repo), "write-tree"],
                                                   env=index_env, text=True).strip()
indent_base = subprocess.check_output(["git", "-C", str(indent_repo), "commit-tree", indent_trees["base"],
                                       "-m", "indent base"], text=True).strip()
indent_accepted = subprocess.check_output(["git", "-C", str(indent_repo), "commit-tree", indent_trees["reviewed"],
                                           "-p", indent_base, "-m", "reviewed indentation"], text=True).strip()
indent_published = subprocess.check_output(["git", "-C", str(indent_repo), "commit-tree", indent_trees["published"],
                                            "-p", indent_base, "-m", "different indentation"], text=True).strip()
subprocess.run(["git", "-C", str(indent_repo), "update-ref", "refs/heads/main", indent_published], check=True)
indent_patch = subprocess.check_output(["git", "-C", str(indent_repo), "diff", "--binary", "--no-renames",
                                        indent_base, indent_accepted, "--"])
published_patch = subprocess.check_output(["git", "-C", str(indent_repo), "diff", "--binary", "--no-renames",
                                           indent_base, indent_published, "--"])
assert saved._patch_id(indent_repo, indent_patch) == saved._patch_id(indent_repo, published_patch)
try:
    saved._publication_contains_acceptance(
        indent_repo, indent_published,
        {"commit": indent_accepted, "tree": indent_trees["reviewed"], "base": indent_base}, indent_patch)
    raise AssertionError("a same-patch-id indentation change was accepted as the reviewed patch")
except ValueError as error:
    assert "does not contain the accepted commit" in str(error), error
# A rewritten commit whose exact tree comes from replaying the reviewed patch is valid.
rebase_repo = temporary / "accept rebase clone without accepted commit"
subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(accept_repo), str(rebase_repo)], check=True)
for key, value in (("user.name", "rebase fixture"), ("user.email", "rebase@example.com")):
    subprocess.run(["git", "-C", str(rebase_repo), "config", key, value], check=True)
rebase_base_index = temporary / "accept-rebase-base-index"
rebase_base_env = dict(os.environ, GIT_INDEX_FILE=str(rebase_base_index))
subprocess.run(["git", "-C", str(rebase_repo), "read-tree", accept_base.decode()],
               env=rebase_base_env, check=True)
rebase_blob = subprocess.check_output(["git", "-C", str(rebase_repo), "hash-object", "-w", "--stdin"],
                                     input=b"rebased context\n").decode().strip()
subprocess.run(["git", "-C", str(rebase_repo), "update-index", "--add", "--cacheinfo",
                "100644," + rebase_blob + ",rebased-only.txt"], env=rebase_base_env, check=True)
rebase_base_tree = subprocess.check_output(["git", "-C", str(rebase_repo), "write-tree"],
                                          env=rebase_base_env, text=True).strip()
rebase_base = subprocess.check_output(["git", "-C", str(rebase_repo), "commit-tree", rebase_base_tree,
                                       "-p", accept_base.decode(), "-m", "rebased context"],
                                      text=True).strip()
rebase_patch_index = temporary / "accept-rebase-patch-index"
rebase_patch_env = dict(os.environ, GIT_INDEX_FILE=str(rebase_patch_index))
subprocess.run(["git", "-C", str(rebase_repo), "read-tree", rebase_base], env=rebase_patch_env, check=True)
subprocess.run(["git", "-C", str(rebase_repo), "apply", "--cached", "--binary", str(accept_patch)],
               env=rebase_patch_env, check=True)
rebase_tree = subprocess.check_output(["git", "-C", str(rebase_repo), "write-tree"],
                                      env=rebase_patch_env, text=True).strip()
rebase_sha = subprocess.check_output(["git", "-C", str(rebase_repo), "commit-tree", rebase_tree,
                                     "-p", rebase_base, "-m", "rebased reviewed patch"],
                                    text=True).strip()
saved._publication_contains_acceptance(rebase_repo, rebase_sha, accept_record, accept_patch.read_bytes())
receipt = {"schema": 1, "state": "passed", "sha": accept_sha, "remote": "origin", "target": "main",
           "gate": {"rc": 0, "command": "true"}, "push": {"rc": 0, "moved": 1, "base": accept_base.decode()},
           "ci": {"conclusion": "success"}}
(receipt_dir / (accept_sha + "-fixture.json")).write_text(json.dumps(receipt))
subprocess.run(["git", "-C", str(accept_execution), "update-ref", "refs/remotes/origin/main", accept_sha], check=True)
# A live review defers a landed finalize before it can finish the plan or write
# any result records/artifacts.
landed_review = accept_repo / ".oms/locks/canonical-sync.d/review-live-landed-fixture"
landed_review.parent.mkdir(parents=True, exist_ok=True)
landed_owner = subprocess.Popen(
    ["bash", "-c", review_owner_script, "canonical-review-owner", str(root), str(accept_repo), landed_review.name],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
assert landed_owner.stdout.readline().strip() == "ready"
(landed_review / "source").write_text(str(accept_execution.resolve()) + "\n")
plan_before_deferred = json.loads(accept_call("agent-plan", "show", "--id", "accept-task",
                                               execution=accept_execution).stdout)
plan_bytes_before_deferred = (accept_repo / ".oms/plan/tasks.json").read_bytes()
result_rows_before = [row for row in saved._records(accept_repo)[1]
                      if row.get("kind") == "panel-result" and row.get("task_id") == "accept-task"]
result_artifacts_dir = accept_repo / ".oms/artifacts/panel-results"
result_artifacts_before = {path.name: path.read_bytes() for path in result_artifacts_dir.glob("result-*.json")}
try:
    deferred_landed = panel_with_review_wait(["--repo", str(accept_execution), *accept_finalize], 2,
                                             cwd=accept_repo, env=accept_env, timeout=90)
    assert deferred_landed.returncode == 75 and landed_review.name in deferred_landed.stderr, deferred_landed.stderr
    plan_after_deferred = json.loads(accept_call("agent-plan", "show", "--id", "accept-task",
                                                  execution=accept_execution).stdout)
    assert plan_after_deferred.get("state") == plan_before_deferred.get("state"), \
        ("deferred landed finalize changed plan state", plan_before_deferred, plan_after_deferred)
    assert (accept_repo / ".oms/plan/tasks.json").read_bytes() == plan_bytes_before_deferred, \
        "deferred landed finalize changed the plan file"
    assert [row for row in saved._records(accept_repo)[1]
            if row.get("kind") == "panel-result" and row.get("task_id") == "accept-task"] == result_rows_before, \
        "deferred landed finalize changed finalize records"
    assert {path.name: path.read_bytes() for path in result_artifacts_dir.glob("result-*.json")} == \
        result_artifacts_before, "deferred landed finalize wrote result artifacts"
finally:
    landed_owner.communicate("release\n", timeout=10)
accept_admission_hook.write_text("printf 'verification failure\\n' > value.txt\n", encoding="utf-8")
# An untracked file makes the checkout unreusable, so the verifier (and its hook failure) still runs.
reuse_blocker = accept_execution / "reuse-blocker.txt"
reuse_blocker.write_text("untracked\n")
failed = accept_call("panel", *accept_finalize, execution=accept_execution, check=False)
reuse_blocker.unlink()
accept_admission_hook.unlink()
(accept_execution / "value.txt").write_text("old\naccepted\n")
assert failed.returncode != 0 and accept_sha in failed.stderr and "finalization failed" in failed.stderr
assert "retry only --finalize after confirming exact completion" in failed.stderr
finished_task = json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout)
assert finished_task["state"] == "done", "finalize ran before plan finish"
assert list((accept_repo / ".oms/artifacts/panel-results").glob("verify-*.json"))
assert not list((accept_execution / ".oms/artifacts/panel-results").glob("verify-*.json")), "diagnostics left canonical state"
finalized = json.loads(accept_call("panel", *accept_finalize, execution=accept_execution).stdout)
assert finalized["outcome"] == "accepted" and saved.result_matches_plan(accept_repo, "accept-task", finalized)
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout) == finished_task
assert json.loads(accept_call("panel", *accept_finalize, execution=accept_execution).stdout)["revision"] == finalized["revision"]
auto_finalize = accept_finalize[:-2]
automatic = json.loads(accept_call("panel", *auto_finalize, execution=accept_execution).stdout)
assert len(automatic["evidence"]) == 1, automatic
assert automatic["evidence"][0]["path"].startswith(".oms/artifacts/admit/"), automatic
assert automatic["evidence"][0] == saved._relative(accept_repo, automatic["evidence"][0]["path"])
assert finalized["evidence"][0]["path"] == ".oms/artifacts/fixture/accept.patch", finalized
# Finalize reuses the admission pass when the landed tree is exactly base + patch and clean.
assert finalized["verification"]["reused_admission"].startswith(".oms/artifacts/admit/"), finalized
assert finalized["verification"]["status"] == "passed" and finalized["verification"]["exit"] == 0, finalized
_, _, reuse_rows, _, _, reuse_admissions = saved._finalize_snapshot(accept_repo, "codex", "accept-task", None, [])
def reusable(execution=accept_execution, rows=reuse_rows, verify=accept_verify):
    return saved._reusable_admission(accept_repo, execution, rows, reuse_admissions, "accept-task", verify)
assert reusable() == finalized["verification"]["reused_admission"]
assert reusable(verify=accept_verify + " ") is None and reusable(verify="") is None
reuse_clone = temporary / "accept-reuse-clone"
subprocess.run(["git", "clone", "-q", str(accept_execution), str(reuse_clone)], check=True)
assert reusable(reuse_clone) is not None
(reuse_clone / "untracked.txt").write_text("x\n")
assert reusable(reuse_clone) is None, "an untracked file still reused the pass"
(reuse_clone / "untracked.txt").unlink()
(reuse_clone / "value.txt").write_text("dirty\n")
assert reusable(reuse_clone) is None, "a modified tracked file still reused the pass"
subprocess.run(["git", "-C", str(reuse_clone), "checkout", "-q", "--", "value.txt"], check=True)
assert reusable(reuse_clone) is not None
(reuse_clone / "value.txt").write_text("extra commit\n")
subprocess.run(["git", "-C", str(reuse_clone), "-c", "user.name=t", "-c", "user.email=t@example.invalid",
                "commit", "-q", "-am", "extra"], check=True)
assert reusable(reuse_clone) is None, "HEAD moved past base + patch still reused the pass"
assert any(row.get("kind") == "patch-admit" for row in reuse_rows), "fixture has no admission row"
assert reusable(rows=[row for row in reuse_rows if row.get("kind") != "patch-admit"]) is None
assert reusable(rows=[dict(row, artifact_sha256="0" * 64) if row.get("kind") == "patch-admit" else row
                      for row in reuse_rows]) is None
assert reusable(rows=[dict(row, exit=1) if row.get("kind") == "patch-admit" else row
                      for row in reuse_rows]) is None
assert saved._reusable_admission(accept_repo, accept_execution, reuse_rows, [], "accept-task", accept_verify) is None
# Report text outside the header block is candidate-controlled and never decides reuse.
admit_row = next(row for row in reuse_rows if row.get("kind") == "patch-admit")
admit_text = (accept_repo / admit_row["artifact"]).read_text(encoding="utf-8")
admit_fields = dict(re.findall(r"^- (\w+): (\S+)$", admit_text.split("\n## ", 1)[0], re.M))
def forged_rows(text, name):
    forged = accept_repo / ".oms/artifacts/admit" / name
    forged.write_text(text, encoding="utf-8")
    digest = hashlib.sha256(text.encode("utf-8")).hexdigest()
    return [dict(row, artifact=".oms/artifacts/admit/" + name, artifact_sha256=digest)
            if row is admit_row else row for row in reuse_rows]
verify_line = "- verify_sha256: " + admit_fields["verify_sha256"]
base_line = "- base: " + admit_fields["base"]
tail = "\n\n## Verify output (tail)\n\n```\n%s\n```\n"
assert verify_line in admit_text and base_line in admit_text
assert reusable(rows=forged_rows(admit_text.replace(verify_line, "- verify_sha256: " + "0" * 64) + tail % verify_line,
                                 "forged-verify.md")) is None, "a forged verify_sha256 tail line reused the pass"
assert reusable(rows=forged_rows(admit_text.replace(base_line, "- base: 0000000") + tail % base_line,
                                 "forged-base.md")) is None, "a forged base tail line reused the pass"
assert reusable(rows=forged_rows(admit_text.replace(base_line, base_line + "\n" + base_line),
                                 "duplicate-key.md")) is None, "a duplicated header key reused the pass"
assert "- verification: command" in admit_text
assert reusable(rows=forged_rows(admit_text.replace("- verification: command", "- verification: skipped"),
                                 "no-command.md")) is None

# A retry still requires the identical publication proof and completion.
wrong_completion = list(accept_finalize)
wrong_completion[wrong_completion.index("--landed-commit") + 1] = accept_base.decode()
assert accept_call("panel", *wrong_completion, execution=accept_execution, check=False).returncode != 0
receipt_path = receipt_dir / (accept_sha + "-fixture.json")
receipt_hidden = receipt_dir / "hidden-fixture.json"
receipt_path.rename(receipt_hidden)
try:
    missing_proof = accept_call("panel", *accept_finalize, execution=accept_execution, check=False)
    assert missing_proof.returncode != 0 and "finish failed" in missing_proof.stderr
finally:
    receipt_hidden.rename(receipt_path)
assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task").stdout) == finished_task
# END reviewed-result acceptance composition.
# Finalize does not hold the shared results lock while a verifier runs.
import threading
slow_started, release_slow = threading.Event(), threading.Event()
slow_errors = []
def controlled_verifier(repo, command, timeout):
    if command == "slow":
        slow_started.set()
        if not release_slow.wait(5):
            raise AssertionError("slow verifier was not released")
    return {"exit": 0}
def finalize_slow():
    try:
        saved.finalize(project, "codex", "finalize-slow-a", str(summary), "completed", "slow")
    except Exception as error:
        slow_errors.append(error)
with patch.dict(os.environ, environment, clear=True), patch.object(saved, "_register"), \
        patch.object(saved, "_plan_hint"), patch.object(saved, "_run_verifier", side_effect=controlled_verifier):
    slow_thread = threading.Thread(target=finalize_slow)
    slow_thread.start()
    assert slow_started.wait(2), "slow verifier did not start"
    try:
        other_result = saved.finalize(project, "codex", "finalize-fast-b", str(summary), "completed", "fast")
        assert other_result["outcome"] == "completed" and slow_thread.is_alive(), \
            "task B waited for task A verifier"
    finally:
        release_slow.set()
        slow_thread.join(5)
    assert not slow_thread.is_alive() and not slow_errors, slow_errors

timeout_args = list(final_args)
timeout_args[timeout_args.index("--task-id") + 1] = "finalize-timeout"
timeout_args[timeout_args.index("--outcome") + 1] = "completed"
timeout_args += ["--verify-timeout", "1"]
timeout_args[timeout_args.index("--verify") + 1] = "printf before-timeout; sleep 5"
timeout_result = subprocess.run(timeout_args, cwd=project, env=environment, capture_output=True, text=True)
assert timeout_result.returncode != 0 and "verifier timed out after 1 s" in timeout_result.stderr, timeout_result.stderr
assert "Traceback" not in timeout_result.stderr, timeout_result.stderr
assert private_diagnostics(timeout_result.stderr)["stdout"]["text"] == "before-timeout"
assert not any(row.get("kind") == "panel-result" and row.get("task_id") == "finalize-timeout"
               for row in saved._records(project)[1]), "timed-out verifier wrote a result"

drift_args = list(final_args)
drift_args[drift_args.index("--task-id") + 1] = "finalize-evidence-drift"
drift_args[drift_args.index("--outcome") + 1] = "completed"
drift_args[drift_args.index("--verify") + 1] = "printf changed > value.txt"
evidence_before = (project / "value.txt").read_bytes()
try:
    drift_result = subprocess.run(drift_args, cwd=project, env=environment, capture_output=True, text=True)
    assert drift_result.returncode != 0 and "evidence" in drift_result.stderr and "changed during verification" in drift_result.stderr, \
        drift_result.stderr
    assert not any(row.get("kind") == "panel-result" and row.get("task_id") == "finalize-evidence-drift"
                   for row in saved._records(project)[1]), "changed evidence wrote a result"
finally:
    (project / "value.txt").write_bytes(evidence_before)
def exercise_auto_admission_evidence():
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        repo = Path(directory)
        reports = repo / ".oms/artifacts/admit"
        reports.mkdir(parents=True)
        artifact = reports / "admit-one.md"
        artifact.write_text("admitted\n")
        digest = hashlib.sha256(artifact.read_bytes()).hexdigest()
        landed = {"patch_sha256": "a" * 64, "task_id": "task", "plan_id": "plan"}
        row = dict(landed, kind="patch-admit", exit=0, artifact=".oms/artifacts/admit/admit-one.md",
                   artifact_sha256=digest)
        expected = [saved._relative(repo, row["artifact"])]
        assert saved._automatic_admission_evidence(repo, [row], [landed], "task") == expected
        for invalid in ([], [dict(row, artifact_sha256="0" * 64)], [dict(row, exit=1)],
                        [dict(row, patch_sha256="")], [dict(row, artifact="")], [row, dict(row, artifact=".oms/artifacts/admit/admit-two.md")]):
            (reports / "admit-two.md").write_text("admitted\n")
            try:
                saved._automatic_admission_evidence(repo, invalid, [landed], "task")
                raise AssertionError("invalid or ambiguous admission accepted")
            except ValueError as error:
                assert "--evidence" in str(error), error
        rows = []
        landed_many = []
        for number in range(9):
            path = reports / ("admit-%s.md" % number)
            path.write_text("admitted %s\n" % number)
            item = dict(landed, patch_sha256=str(number) * 64)
            landed_many.append(item)
            rows.append(dict(item, kind="patch-admit", exit=0,
                             artifact=".oms/artifacts/admit/" + path.name,
                             artifact_sha256=hashlib.sha256(path.read_bytes()).hexdigest()))
        assert len(saved._automatic_admission_evidence(repo, rows[:8], landed_many[:8], "task")) == 8
        try:
            saved._automatic_admission_evidence(repo, rows, landed_many, "task")
            raise AssertionError("evidence limit exceeded")
        except ValueError as error:
            assert "--evidence" in str(error), error
        try:
            saved._automatic_admission_evidence(repo, [row], [], "task")
            raise AssertionError("empty admission accepted")
        except ValueError as error:
            assert "--evidence" in str(error), error

exercise_auto_admission_evidence()

# BEGIN patch-scope finalize regression (also runnable as an isolated case).
(project / ".oms/artifacts").mkdir(parents=True, exist_ok=True)
scope_patch = project / ".oms/artifacts/scope-result.patch"
scope_worker = {"attempt_id": "att_scope_write", "task_id": "scope-result", "parent_attempt_id": "scope-main",
                "refs": {"panel_role": "worker", "panel_access": "write", "panel_room_id": "scope-room",
                         "panel_room_participant": "scope-worker"}}

def scope_evidence(body, reviewed=True):
    scope_patch.write_bytes(body)
    digest = hashlib.sha256(body).hexdigest()
    rows = [{"kind": "delegate", "task_id": "scope-result", "attempt_id": "att_scope_write",
             "patch": ".oms/artifacts/scope-result.patch", "patch_sha256": digest, "exit": 0},
            {"kind": "patch-land", "task_id": "scope-result", "patch_sha256": digest, "exit": 0}]
    if reviewed:
        rows.append({"kind": "review-outcome", "task_id": "scope-result", "exit": 0,
                     "review": {"gate_verify_exit": 0, "candidate": {"patch_sha256": digest},
                                "seats": [{"verdict": "pass"}]}})
    return rows

extra_patch = (b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-old\n+new\n"
               b"diff --git a/other.txt b/other.txt\n--- /dev/null\n+++ b/other.txt\n@@ -0,0 +1 @@\n+extra\n")
scope_member = {"participants": [{"participant": "scope-worker", "owns": ["value.txt"]}]}
with patch.dict(os.environ, environment, clear=True), \
        patch.object(saved, "_plan_bindings", return_value={}), \
        patch.object(saved, "_register"), patch.object(saved, "_plan_hint"), \
        patch.object(saved.room, "status", return_value=scope_member):
    simple_patch = extra_patch.split(b"diff --git a/other.txt")[0]
    # A digest-bound landing in the state repo must be present in the verifier tree.
    landing = project / ".oms/landing-patches/land-scope.patch"
    landing.parent.mkdir(parents=True, exist_ok=True)
    # A real landing publication is a hard link owned by the capture store (link count 2).
    capture_store = runpy.run_path(str(root / "scripts/lib/landing-capture.py"))
    capture_source = temporary / "landing-source.patch"
    capture_source.write_bytes(simple_patch)
    capture_store["publish"](str(project), capture_store["allocate"](
        str(project), str(capture_source), "land-scope", hashlib.sha256(simple_patch).hexdigest()), str(landing))
    assert landing.stat().st_nlink == 2, landing.stat().st_nlink
    landed_rows = scope_evidence(simple_patch)
    landed_rows[1]["patch"] = ".oms/landing-patches/land-scope.patch"
    landing_execution = temporary / "landing-verifier-checkout"
    subprocess.run(["git", "clone", "-q", str(project), str(landing_execution)], check=True)
    with patch.object(saved, "_records", return_value=([scope_worker], landed_rows)), \
            patch.object(saved, "_execution_repo", return_value=landing_execution), \
            patch.object(saved, "_run_verifier", return_value={"exit": 0}) as verifier, \
            patch.object(saved, "_write") as write:
        for path in ("", "0", "../outside.patch", str(landing), ".oms/landing-patches/missing.patch"):
            landed_rows[1]["patch"] = path
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                                  ["value.txt"])["outcome"] == "accepted", path
        landed_rows[1]["patch"] = ".oms/landing-patches/land-scope.patch"
        landing.write_bytes(b"different bytes\n")
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                              ["value.txt"])["outcome"] == "accepted"
        landing.write_bytes(simple_patch)
        verifier.reset_mock()
        write.reset_mock()
        landing_stderr = io.StringIO()
        try:
            with redirect_stderr(landing_stderr):
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("accepted a verifier tree missing the landing")
        except ValueError as error:
            assert landed_rows[1]["patch"] in str(error) and "--repo WORKTREE" in str(error), error
        assert landing_stderr.getvalue() == "", landing_stderr.getvalue()
        verifier.assert_not_called()
        write.assert_not_called()
        # Patches over the 1 MiB evidence ceiling still bind admission and run the check.
        big_patch = (b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1,2 @@\n"
                     b"-old\n+new\n+" + b"x" * (1 << 20) + b"\n")
        big_rows = scope_evidence(big_patch)
        capture_source.write_bytes(big_patch)
        capture_store["publish"](str(project), capture_store["allocate"](
            str(project), str(capture_source), "land-big", hashlib.sha256(big_patch).hexdigest()),
            str(project / ".oms/landing-patches/land-big.patch"))
        big_rows[1]["patch"] = ".oms/landing-patches/land-big.patch"
        with patch.object(saved, "_records", return_value=([scope_worker], big_rows)):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("accepted a verifier tree missing a landing over 1 MiB")
            except ValueError as error:
                assert "land-big.patch" in str(error), error
        scope_evidence(simple_patch)
        write.assert_not_called()
        run_command = subprocess.run
        def unavailable_check(args, **kwargs):
            if args == ["git", "apply", "--check"]:
                raise subprocess.TimeoutExpired(args, 60)
            return run_command(args, **kwargs)
        with patch.object(subprocess, "run", side_effect=unavailable_check):
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                                  ["value.txt"])["outcome"] == "accepted"
        for invalid in (b"", b"malformed patch\n"):
            landing.write_bytes(invalid)
            invalid_rows = scope_evidence(invalid)
            invalid_rows[1]["patch"] = landed_rows[1]["patch"]
            with patch.object(saved, "_records", return_value=([scope_worker], invalid_rows)):
                assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                                      ["value.txt"])["outcome"] == "accepted"
        scope_evidence(simple_patch)
        landing.write_bytes(simple_patch)
        (landing_execution / "value.txt").write_text("conflict\n")
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                              ["value.txt"])["outcome"] == "accepted"
        (landing_execution / "value.txt").write_text("old\n")
        verifier.reset_mock()
        subprocess.run(["git", "apply"], cwd=landing_execution, input=simple_patch, check=True)
        try:
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                                  ["value.txt"])["outcome"] == "accepted"
            verifier.assert_called_once_with(landing_execution, "true", 1800, evidence_repo=project)
        finally:
            subprocess.run(["git", "apply", "--reverse"], cwd=landing_execution, input=simple_patch, check=True)
    # BEGIN missing-admission diagnostic regression.
    diagnostic_workers = [dict(scope_worker, attempt_id=name) for name in ("att_missing_1", "att_landed", "att_missing_2")]
    with patch.object(saved, "_worker_owns", return_value=[]):
        try:
            saved._review_preflight(project, "scope-result", diagnostic_workers, [], [None, {"patch_sha256": "x"}, None])
            raise AssertionError("missing admissions accepted")
        except ValueError as error:
            message = str(error)
            assert "matching landed patch" in message and "att_missing_1, att_missing_2" in message, message
            assert "att_landed" not in message and "oms patch-land" in message, message
            assert "worker's own patch" in message and "exact delegate patch bytes" in message, message
            assert "landing is recorded" in message and "--outcome completed" in message, message
            assert "state repository" in message and "independent clone" in message, message
            assert "this checkout" in message and "oms scratch-worktree" in message, message
            assert "\n" not in message, message
    # END missing-admission diagnostic regression.
    unreviewed_rows = scope_evidence(simple_patch, reviewed=False)
    with patch.object(saved, "_records", return_value=([scope_worker], unreviewed_rows)):
        try:
            saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("unreviewed landed write patch accepted")
        except ValueError as error:
            assert "att_scope_write" in str(error) and "run the review" in str(error) \
                and "finalize completed" in str(error), error
        assert saved.finalize(project, "codex", "scope-result", str(summary), "completed")["outcome"] == "completed"
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(simple_patch))):
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true",
                              ["value.txt"])["outcome"] == "accepted"
    reviews = scope_evidence(simple_patch)
    passing_review = reviews[-1]
    failing_review = dict(passing_review, exit=1,
                         review=dict(passing_review["review"], gate_verify_exit=1,
                                     seats=[{"verdict": "fail"}]))
    for ordered_reviews, should_accept in (([passing_review, failing_review], False),
                                           ([failing_review, passing_review], True)):
        ordered_rows = reviews[:-1] + ordered_reviews
        with patch.object(saved, "_records", return_value=([scope_worker], ordered_rows)):
            try:
                result = saved.finalize(project, "codex", "scope-result", str(summary), "accepted",
                                        "true", ["value.txt"])
            except ValueError as error:
                assert not should_accept and "lack a passing review gate" in str(error), error
            else:
                assert should_accept and result["outcome"] == "accepted", result
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(extra_patch))):
        changes = saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]
        assert changes["out_of_scope"] == {"count": 1, "paths": ["other.txt"]}, changes
        for task, main, worker_task in (("scope-result", None, "scope-result"),
                                        ("scope-result", "scope-main", "scope-result"),
                                        ("whole-scope-main", "scope-main", None)):
            worker = dict(scope_worker, task_id=worker_task)
            attempts = [worker] + ([{"attempt_id": main, "tool": "panel-main", "provider": "codex"}] if main else [])
            rows = [dict(row, task_id=worker_task) for row in scope_evidence(extra_patch)]
            with patch.object(saved, "_records", return_value=(attempts, rows)), \
                    patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": main} if main else {}):
                try:
                    saved.finalize(project, "codex", task, str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("out-of-scope write patch accepted")
                except ValueError as error:
                    assert "1 path(s) outside its declared scope" in str(error) and "other.txt" in str(error), error
                assert saved.finalize(project, "codex", task, str(summary), "completed")["outcome"] == "completed"
        # Another task's drift and missing landing cannot veto this task under the same main.
        attempts = [scope_worker, {"attempt_id": "scope-main", "tool": "panel-main", "provider": "codex"}]
        with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": "scope-main"}):
            rows = scope_evidence(extra_patch)
            for evidence_rows in (rows, rows[:1]):
                with patch.object(saved, "_records", return_value=(attempts, evidence_rows)):
                    result = saved.finalize(project, "codex", "other-task", str(summary), "accepted", "true", ["value.txt"])
                    assert result["outcome"] == "accepted" and not result["admission_event_ids"], result
            # An unassigned worker must still supply matching landed evidence.
            attempts[0] = dict(scope_worker, task_id=None)
            rows = [dict(row, task_id=None) for row in scope_evidence(extra_patch.split(b"diff --git a/other.txt")[0])]
            with patch.object(saved, "_records", return_value=(attempts, rows[:1])):
                try:
                    saved.finalize(project, "codex", "other-task", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("unassigned write worker accepted without landing")
                except ValueError as error:
                    assert "matching landed patch" in str(error), error
            with patch.object(saved, "_records", return_value=(attempts, rows)):
                assert saved.finalize(project, "codex", "other-task", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
            changed = [dict(attempts[0], refs=dict(scope_worker["refs"], panel_room_participant="changed")), attempts[1]]
            with patch.object(saved, "_records", side_effect=[(attempts, rows), (changed, rows)]):
                try:
                    saved.finalize(project, "codex", "other-task", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("unassigned worker changed during verification")
                except ValueError as error:
                    assert "related attempts changed" in str(error), error
        scope_evidence(extra_patch)
        for owns in ([], None):
            scope_member["participants"][0]["owns"] = owns
            assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"]["count"] == 0
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
    scope_member["participants"][0]["owns"] = ["docs/*.md"]
    glob_patch = b"diff --git a/docs/readme.md b/docs/readme.md\n--- /dev/null\n+++ b/docs/readme.md\n@@ -0,0 +1 @@\n+doc\n"
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(glob_patch))):
        assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"]["count"] == 0
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
    rename_patch = b"diff --git a/docs/readme.md b/outside.md\nsimilarity index 100%\nrename from docs/readme.md\nrename to outside.md\n"
    for owns, outside in ((["docs/*.md"], "outside.md"), (["outside.md"], "docs/readme.md")):
        scope_member["participants"][0]["owns"] = owns
        with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(rename_patch))):
            assert saved.results(project, "scope-result")["rows"][0]["calls"][0]["changes"]["out_of_scope"] == {"count": 1, "paths": [outside]}
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("out-of-scope rename accepted")
            except ValueError as error:
                assert "outside its declared scope" in str(error) and outside in str(error), error
    many_patch = b"".join(("diff --git a/file%d b/file%d\nGIT binary patch\nliteral 0\n\n" % (n, n)).encode() for n in range(25))
    assert saved._patch_paths(many_patch) == ["file%d" % n for n in range(25)]
    scope_patch.write_bytes(many_patch)
    assert saved._patch_paths(scope_patch) == saved._patch_paths(many_patch)
    assert saved._scope_drift(saved._patch_paths(many_patch), ["file0"])[-1] == "file24"
    long_name = "docs/" + "x" * 180 + ".md"
    assert saved._patch_paths(("diff --git a/%s b/%s\n" % (long_name, long_name)).encode()) == [long_name]
    quoted_patch = b'diff --git "a/docs/caf\\303\\251.md" "b/docs/caf\\303\\251.md"\nGIT binary patch\n'
    assert saved._patch_paths(quoted_patch) == ["docs/café.md"]
    spaced = "docs/a b/read me.md"
    assert saved._patch_paths(("diff --git a/%s b/%s\nGIT binary patch\n" % (spaced, spaced)).encode()) == [spaced]
    plain = b"--- a/first.txt\n+++ b/first.txt\n@@ -1 +1 @@\n--- content\n+++ content\n--- a/second.txt\n+++ b/second.txt\n@@ -0,0 +1 @@\n+new\n"
    assert saved._patch_paths(plain) == ["first.txt", "second.txt"]
    with patch.object(saved, "verified_patch", return_value=plain):
        stats = saved._diffstat(project, {})
        assert stats == {"files": [["first.txt", 1, 1], ["second.txt", 1, 0]], "count": 2, "added": 2, "removed": 1}, stats
    with patch.object(saved, "verified_patch", return_value=many_patch):
        stats = saved._diffstat(project, {})
        assert stats["count"] == 25 and len(stats["files"]) == 20, stats
    scope_member["participants"][0]["owns"] = ["file0"]
    with patch.object(saved, "_records", return_value=([scope_worker], scope_evidence(many_patch))):
        drift = saved.results(project)["rows"][0]["calls"][0]["changes"]["out_of_scope"]
        assert drift == {"count": 24, "paths": ["file%d" % n for n in range(1, 9)]}, drift
        try:
            saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("drift after display file cap accepted")
        except ValueError as error:
            assert "24 path(s) outside its declared scope" in str(error) and "file8" in str(error) and "file9" not in str(error), error
    assert saved._patch_paths(b"--- a/deleted.txt\n+++ /dev/null\n@@ -1 +0,0 @@\n-old\n") == ["deleted.txt"]
# Failed/withdrawn history is accounted for only by a later landed retry.
retry_patch = extra_patch.split(b"diff --git a/other.txt")[0]
retry_rows = scope_evidence(retry_patch)
retry_rows[1]["event_id"] = "evt_retry_landed"
retry_worker = dict(scope_worker, created_at="2026-10-16T00:00:02Z", state="review",
                    refs=dict(scope_worker["refs"], panel_purpose="implement"))
failed_worker = dict(retry_worker, attempt_id="att_retry_failed", state="failed",
                     created_at="2026-10-16T00:00:01Z")
failed_row = {"kind": "delegate", "task_id": "scope-result", "attempt_id": failed_worker["attempt_id"], "exit": 7}
with patch.dict(os.environ, environment, clear=True), patch.object(saved, "_register"), \
        patch.object(saved, "_plan_hint"), patch.object(saved, "_worker_owns", return_value=["value.txt"]):
    for state in ("failed", "timed_out", "cancelled", "abandoned"):
        previous = dict(failed_worker, state=state, refs=dict(failed_worker["refs"], panel_alias="W2"))
        with patch.object(saved, "_records", return_value=([previous, retry_worker], [failed_row] + retry_rows)):
            accepted_retry = saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            assert accepted_retry["admission_event_ids"] == ["evt_retry_landed"], accepted_retry
            assert accepted_retry["superseded_attempts"] == [{"attempt_id": previous["attempt_id"],
                "superseded_by": retry_worker["attempt_id"], "reason": state}], accepted_retry
            with patch.object(saved, "_load_result", return_value=accepted_retry):
                report_rows = [failed_row] + retry_rows + [{"kind": "panel-result", "task_id": "scope-result"}]
                with patch.object(saved, "_records", return_value=([previous, retry_worker], report_rows)):
                    view = saved.results(project, "scope-result")["rows"][0]
                    assert view["representative_state"] == "main_decision accepted" and view["calls"][0]["superseded"], view
            settled_retry = review_settlement._settle(project, [previous], [], {"scope-main"},
                {"scope-result": (accepted_retry, {"ts": "2026-10-17T00:00:00Z"})})
            assert not settled_retry["pending"] and settled_retry["superseded"][0]["attempt"] == previous["attempt_id"]
    previous = dict(failed_worker, state="review")
    # A stopped call with controller-recorded empty recovery is not a patch candidate.
    import panel_salvage
    empty_worker = dict(failed_worker, parent_attempt_id="different-main")
    empty_path = project / panel_salvage._empty_provenance_path("scope-result", empty_worker["attempt_id"])
    empty_path.parent.mkdir(parents=True, exist_ok=True)
    empty_digest = panel_salvage._empty_provenance_identity("scope-result", empty_worker["attempt_id"])
    empty_record = {"schema": 1, "payload_sha256": empty_digest, "records": [{"encoding_warnings": [],
        "salvaged_from": {"task": "scope-result", "attempt": empty_worker["attempt_id"],
                          "outcome": "nothing_to_recover", "reason": "arbitrary"}}]}
    with patch.object(saved, "_records", return_value=([empty_worker, retry_worker], [failed_row] + retry_rows)), \
            patch.object(panel_salvage, "_latest", return_value=(str(project / "missing-kept"), empty_worker["attempt_id"], None, project, {}, True)), \
            patch.object(panel_salvage, "_attempt_recovery_evidence", return_value=[]):
        try:
            saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("missing empty salvage accepted")
        except ValueError as error:
            assert "matching landed patch" in str(error), error
        empty_path.write_text(json.dumps(empty_record))
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
        for bad in ({}, dict(empty_record, payload_sha256="0" * 64), dict(empty_record, records=[])):
            empty_path.write_text(json.dumps(bad))
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("invalid empty salvage accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
        for field, value in (("task", "other"), ("attempt", "other"), ("outcome", "recovered")):
            origin = dict(empty_record["records"][0]["salvaged_from"], **{field: value})
            empty_path.write_text(json.dumps(dict(empty_record, records=[{"encoding_warnings": [], "salvaged_from": origin}])))
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("foreign empty salvage accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
        empty_path.write_text(json.dumps(empty_record))
        with patch.object(saved, "_records", return_value=([empty_worker, retry_worker],
                [dict(failed_row, patch="patch", patch_sha256="x")] + retry_rows)), \
                patch.object(saved, "verified_patch", return_value=b"patch"):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("nonempty patch accepted without landing")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
        with patch.object(panel_salvage, "_attempt_recovery_evidence", return_value=["recoverable"]):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("recoverable attempt accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
    # Recovery must inspect the recorded execution clone, not the state clone.
    recovery_clone = temporary / "empty-recovery-clone"
    recovery_root = temporary / "empty-recovery-worktrees"
    recovery_tree = recovery_root / "oh-my-setting-delegate.fixture" / "wt"
    recovery_tree.parent.mkdir(parents=True)
    subprocess.run(["git", "clone", "-q", str(project), str(recovery_clone)], check=True)
    subprocess.run(["git", "-C", str(recovery_clone), "worktree", "add", "--detach", str(recovery_tree)],
                   check=True, stdout=subprocess.DEVNULL)
    (recovery_tree.parent / ".oh-my-setting-tmp").write_text(
        "kind=oh-my-setting-temp\ntemporary=1\nrepo=%s\nworktree=%s\n" % (recovery_clone, recovery_tree))
    recovery_worker = dict(empty_worker, terminal=True, tool="peer-delegate", refs=dict(empty_worker["refs"],
        panel_location="worktree:oh-my-setting-delegate.fixture/wt", panel_room_id="room-fixture",
        panel_room_participant="call-0123456789abcdef"))
    base = subprocess.check_output(["git", "-C", str(recovery_clone), "rev-parse", "HEAD"], text=True).strip()
    recovery_row = dict(failed_row, base_sha=base)
    anchor = {"call": {"participant": "call-0123456789abcdef", "room": "room-fixture",
                       "task_id": "scope-result", "attempt_id": recovery_worker["attempt_id"]},
              "execution_repo": str(recovery_clone)}
    def recovery_query(repo, verb, *args):
        return [recovery_worker, retry_worker] if verb == "agent-events" else {"rows": [recovery_row] + retry_rows}
    with patch.dict(os.environ, {"OMS_DELEGATE_WORKTREE_ROOT": str(recovery_root)}), \
            patch.object(saved, "_records", return_value=([recovery_worker, retry_worker], [recovery_row] + retry_rows)), \
            patch.object(saved, "_query", side_effect=recovery_query), \
            patch.object(panel_salvage.dispatch_supervisor, "read", return_value=anchor):
        assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
        (recovery_tree / "value.txt").write_text("changed after empty salvage\n")
        for execution in (str(recovery_clone), str(temporary / "missing-execution"), "", "malformed"):
            anchor["execution_repo"] = execution
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("recoverable or unavailable execution accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
        anchor["execution_repo"] = str(recovery_clone)
        (recovery_tree / "value.txt").write_text((recovery_clone / "value.txt").read_text())
        for attempt_ref in ("", "other", None):
            anchor["call"]["attempt_id"] = attempt_ref
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("unvalidated execution owner accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
        anchor["call"]["attempt_id"] = recovery_worker["attempt_id"]
        recovery_worker = dict(recovery_worker, refs=dict(recovery_worker["refs"]))
        recovery_worker["refs"].pop("panel_location")
        artifact = project / ".oms/artifacts/empty-location.txt"
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.write_text("worktree kept at %s\n" % recovery_tree)
        recovery_row = dict(recovery_row, artifact=".oms/artifacts/empty-location.txt",
                            artifact_sha256=hashlib.sha256(artifact.read_bytes()).hexdigest())
        with patch.object(saved, "_records", return_value=([recovery_worker, retry_worker], [recovery_row] + retry_rows)):
            assert saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])["outcome"] == "accepted"
            (recovery_tree / "value.txt").write_text("changed after empty salvage\n")
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("dirty artifact-only worktree accepted")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
            (recovery_tree / "value.txt").write_text((recovery_clone / "value.txt").read_text())
            for bad_base in (None, "", "0", "malformed", "0" * 40):
                recovery_row = dict(recovery_row, base_sha=bad_base)
                try:
                    saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("unverified kept worktree base accepted")
                except ValueError as error:
                    assert "matching landed patch" in str(error), error
            recovery_row = dict(recovery_row, base_sha=base)
            marker = recovery_tree.parent / ".oh-my-setting-tmp"
            marker_contents = marker.read_bytes()
            try:
                marker.write_bytes(b"malformed")
                try:
                    saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("unverified kept worktree identity accepted")
                except ValueError as error:
                    assert "matching landed patch" in str(error), error
            finally:
                marker.write_bytes(marker_contents)
            actual_run = subprocess.run
            def bad_git_root(args, **kwargs):
                env = kwargs.get("env", {})
                if args[-2:] == ["rev-parse", "--show-toplevel"] and env.get("GIT_CONFIG_NOSYSTEM") == "1":
                    return subprocess.CompletedProcess(args, git_status, stdout=git_root, stderr="")
                return actual_run(args, **kwargs)
            for git_status, git_root in ((1, ""), (0, ""), (0, "malformed\n"), (0, str(project) + "\n")):
                with patch.object(subprocess, "run", side_effect=bad_git_root):
                    try:
                        saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                        raise AssertionError("invalid Git root accepted")
                    except ValueError as error:
                        assert "matching landed patch" in str(error), error
            metadata = recovery_clone / ".git"
            hidden = recovery_clone / "hidden-git-metadata"
            metadata.rename(hidden)
            try:
                try:
                    saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("non-Git execution repository accepted")
                except ValueError as error:
                    assert "matching landed patch" in str(error), error
            finally:
                hidden.rename(metadata)
        artifact.unlink()
    empty_path.unlink()
    continued = dict(retry_worker, refs=dict(retry_worker["refs"], panel_continues=previous["attempt_id"]))
    with patch.object(saved, "_records", return_value=([previous, continued], [failed_row] + retry_rows)):
        result = saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
        assert result["superseded_attempts"] == [{"attempt_id": previous["attempt_id"],
            "superseded_by": continued["attempt_id"], "reason": "continued"}], result
    middle = dict(continued, attempt_id="att_middle", created_at="2026-10-16T00:00:01.5Z")
    last = dict(continued, refs=dict(continued["refs"], panel_continues=middle["attempt_id"]))
    selected, landed, superseded = saved._write_candidates(project, [previous, middle, last], [None, None, {"event_id": "landed"}], [], "codex")
    assert selected == [last] and len(superseded) == 2 and all(item["reason"] == "continued" for item in superseded)
    for ref in (None, "", "malformed", "att_missing"):
        candidate = dict(continued, refs=dict(continued["refs"], panel_continues=ref))
        selected, _, superseded = saved._write_candidates(project, [previous, candidate], [None, {"event_id": "landed"}], [], "codex")
        assert selected == [previous, candidate] and not superseded
    # A -> B -> C retains one participant even after both write parents stop.
    gone = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.read()"], stdin=subprocess.PIPE)
    gone_refs = dict(panel_release.process_refs(gone.pid, "wrapper"), **panel_release.process_refs(gone.pid, "native"))
    gone.communicate(b"", timeout=10)
    old_main = {"attempt_id": "scope-main", "provider": "codex", "tool": "panel-main", "state": "done",
                "terminal": True, "created_at": "2026-10-15T00:00:00Z",
                "refs": dict(gone_refs, panel_host="inline", panel_role="main",
                             panel_room_participant="retry-main-stable", panel_owner="codex")}
    resumed_main = dict(old_main, attempt_id="scope-main-resumed", state="working", terminal=False,
                        created_at="2026-10-16T00:00:00Z")
    resumed_worker = dict(retry_worker, parent_attempt_id=resumed_main["attempt_id"])
    latest_main = dict(resumed_main, attempt_id="scope-main-latest", created_at="2026-10-17T00:00:00Z")
    for current, history in ((resumed_main, [old_main, resumed_main]),
            (latest_main, [old_main, dict(resumed_main, state="done", terminal=True), latest_main])):
        with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": current["attempt_id"]}):
            attempts = history + [failed_worker, resumed_worker]
            with patch.object(saved, "_records", return_value=(attempts, [failed_row] + retry_rows)):
                accepted_retry = saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                assert accepted_retry["main_attempt_id"] == current["attempt_id"]
                assert accepted_retry["admission_event_ids"] == ["evt_retry_landed"]
                assert accepted_retry["superseded_attempts"][0]["attempt_id"] == failed_worker["attempt_id"]
            for previous_main, rows in (
                    (dict(old_main, state="working", terminal=False), retry_rows),
                    (dict(old_main, terminal=None), retry_rows),
                    (dict(old_main, provider="claude"), retry_rows),
                    (dict(old_main, refs=dict(old_main["refs"], panel_room_participant="foreign-main")), retry_rows),
                    (dict(old_main, refs=dict(old_main["refs"], panel_wrapper_identity="")), retry_rows),
                    (dict(old_main, refs=dict(old_main["refs"], panel_native_identity="")), retry_rows),
                    (old_main, retry_rows[:1])):
                attempts = [previous_main] + history[1:] + [failed_worker, resumed_worker]
                with patch.object(saved, "_records", return_value=(attempts, [failed_row] + rows)):
                    try:
                        saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                        raise AssertionError("unverified lineage or unlanded resumed retry was accepted")
                    except ValueError as error:
                        assert "matching landed patch" in str(error), error
            foreign_main = dict(old_main, attempt_id="foreign-main", refs=dict(old_main["refs"],
                                panel_room_participant="foreign-owner"))
            foreign_workers = [dict(worker, parent_attempt_id="foreign-main") for worker in (failed_worker, resumed_worker)]
            with patch.object(saved, "_records", return_value=(history + [foreign_main] + foreign_workers, [failed_row] + retry_rows)):
                try:
                    saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                    raise AssertionError("same-parent retries of a foreign owner escaped admission")
                except ValueError as error:
                    assert "matching landed patch" in str(error), error
    # A live, ambiguous, distinct or unlanded candidate still blocks acceptance.
    for previous, candidate, rows in (
            (dict(failed_worker, state="working"), retry_worker, retry_rows),
            (dict(failed_worker, state="review"), retry_worker, retry_rows),
            (dict(failed_worker, created_at=retry_worker["created_at"]), retry_worker, retry_rows),
            (dict(failed_worker, parent_attempt_id="other-main"), retry_worker, retry_rows),
            (dict(failed_worker, refs=dict(failed_worker["refs"], panel_purpose="different")), retry_worker, retry_rows),
            (failed_worker, retry_worker, retry_rows[:1])):
        with patch.object(saved, "_records", return_value=([previous, candidate], [failed_row] + rows)):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("unaccounted attempt escaped admission")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
    for old_purpose, new_purpose, matches in ((None, None, True), (None, "implement", False),
            ("implement", None, False), ("", "", True), ("", "implement", False)):
        previous = dict(failed_worker, refs=dict(failed_worker["refs"], panel_purpose=old_purpose, panel_alias="W2"))
        candidate = dict(retry_worker, refs=dict(retry_worker["refs"], panel_purpose=new_purpose))
        selected, _, superseded = saved._write_candidates(project, [previous, candidate], [None, {"event_id": "landed"}], [], "codex")
        assert (selected == [candidate] and len(superseded) == 1) if matches else (selected == [previous, candidate] and not superseded)
    for old_scope, new_scope, matches in ((["parser.py"], ["ui.py"], False),
            (["value.txt"], ["value.txt"], True), (["value.txt"], ["value.txt", "other.txt"], True),
            (["value.txt"], ["."], False), (["src/parser.py"], ["src"], False),
            (["src"], ["[s]rc"], False),
            (["src/[ab]/parser.py"], ["src/[ab]"], False),
            (["src/[ab]"], ["src/[ab]"], True), (["src/[ab]/parser.py"], ["src"], False),
            (["./value.txt"], ["value.txt"], False),
            ([str(n) for n in range(16)], [str(n) for n in range(16)], True),
            ([str(n) for n in range(17)], [str(n) for n in range(17)], False),
            (["src/[z-a]/parser.py"], ["src"], False), (["value.txt"], ["[z-a]"], False),
            (["src2/parser.py"], ["src"], False), ([], ["value.txt"], False),
            (["value.txt"], [], False), ([""], ["value.txt"], False),
            ([" "], ["value.txt"], False), ([0], ["value.txt"], False),
            (["../bad"], ["."], False), (["value.txt"], ["../bad"], False),
            (ValueError("unavailable"), ["value.txt"], False),
            (["value.txt"], ValueError("unavailable"), False)):
        def scope_for_attempt(repo, worker):
            scope = old_scope if worker["attempt_id"] == failed_worker["attempt_id"] else new_scope
            if isinstance(scope, Exception):
                raise scope
            return scope
        with patch.object(saved, "_worker_owns", side_effect=scope_for_attempt):
            selected, _, superseded = saved._write_candidates(project, [failed_worker, retry_worker],
                [None, {"event_id": "landed"}], [], "codex")
            assert (selected == [retry_worker] and len(superseded) == 1) if matches else (selected == [failed_worker, retry_worker] and not superseded)
            if not matches and old_scope in (["parser.py"], [], ["value.txt"]) and new_scope in (["ui.py"], [], ["value.txt"]):
                with patch.object(saved, "_records", return_value=([failed_worker, retry_worker], [failed_row] + retry_rows)):
                    try:
                        saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                        raise AssertionError("uncovered attempt escaped admission")
                    except ValueError as error:
                        assert str(error), error
    for old_scope, new_scope in ((["src/[ab]/parser.py"], ["src/[ab]", "value.txt"]),
                                 (["src"], ["[s]rc", "value.txt"])):
        def glob_scope(repo, worker):
            return old_scope if worker["attempt_id"] == failed_worker["attempt_id"] else new_scope
        with patch.object(saved, "_worker_owns", side_effect=glob_scope), \
                patch.object(saved, "_records", return_value=([failed_worker, retry_worker], [failed_row] + retry_rows)):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("distinct scope escaped admission")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
# END patch-scope finalize regression.

whole = json.loads(call(whole_finalize, owner_env))
assert whole["main_attempt_id"] == main_attempts["claude"] and not whole["admission_event_ids"]
joined = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                         "--task-id", "whole-main-result", "--json"]))["rows"][0]
assert not joined["calls"], joined
assert joined["title"] == whole["summary"].split("\n")[0]
assert all(joined[key] is None for key in ("model", "role", "location", "process_state"))
parent_task = {"task_id": "parent-read", "main_attempt_id": "main-parent", "revision": "a" * 24,
               "outcome": "completed", "verification": {"status": "passed"},
               "summary": "Owner summary for the parent read.\nMore detail."}
unrelated_child = {"task_id": "failed-child", "attempt_id": "att_failed_child", "parent_attempt_id": "main-parent",
                   "state": "failed", "refs": {"panel_label": "Unrelated failed write", "panel_model": "gpt-test",
                   "panel_role": "worker", "panel_location": "remote"}}
failed_call = {"kind": "delegate", "task_id": "failed-child", "attempt_id": "att_failed_child", "exit": 1}
unassigned_child = {"attempt_id": "att_unassigned_child", "parent_attempt_id": "main-parent", "refs": {}}
unassigned_call = {"kind": "delegate", "attempt_id": "att_unassigned_child", "exit": 0}
parent_result_row = {"kind": "panel-result", "task_id": "parent-read"}
with patch.object(saved, "_records", return_value=([unassigned_child, unrelated_child],
        [parent_result_row, unassigned_call, failed_call])), \
        patch.object(saved, "_load_result", return_value=parent_task):
    parent = saved.results(project, "parent-read")["rows"][0]
    assert parent["title"] == "Owner summary for the parent read."
    assert parent["issue"] is None and parent["outcome"] == "completed"
    assert parent["verification"]["status"] == "passed"
    assert parent["model"] is None and parent["role"] is None and parent["location"] is None
    assert parent["process_state"] is None and parent["calls"] == [{"kind": "delegate", "exit": 0,
                                                                    "attempt_id": "att_unassigned_child"}]
    assert parent["stages"]["implement"]["attempt_id"] == "att_unassigned_child"
own_task_result = dict(parent_task, task_id="own-task", summary="Own task summary.")
own_attempt = {"task_id": "own-task", "attempt_id": "att_own_task", "parent_attempt_id": "main-parent",
               "state": "done", "refs": {"panel_label": "Own task title", "panel_model": "own-model",
               "panel_role": "reviewer", "panel_location": "local"}}
own_call = {"kind": "call", "task_id": "own-task", "attempt_id": "att_own_task", "exit": 0}
own_call.update(tokens=1820000, provider_usage={"input_tokens": 1700000, "output_tokens": 120000,
                                               "cache_in_input": True})
with patch.object(saved, "_records", return_value=([own_attempt, unrelated_child],
        [{"kind": "panel-result", "task_id": "own-task"}, own_call, failed_call])), \
        patch.object(saved, "_load_result", return_value=own_task_result):
    own_metadata = saved.results(project, "own-task")["rows"][0]
    assert (own_metadata["title"], own_metadata["model"], own_metadata["role"],
            own_metadata["location"], own_metadata["process_state"]) == (
                "Own task title", "own-model", "reviewer", "local", "done")
    assert [call["attempt_id"] for call in own_metadata["calls"]] == ["att_own_task"]
    assert own_metadata["calls"][0]["tokens"] == 1820000
    assert own_metadata["calls"][0]["provider_usage"] == own_call["provider_usage"]
# A task with its own attempts never adopts its main's other children.
call(final_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
own = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                       "--task-id", "panel-council-test", "--json"]))["rows"][0]
assert own["calls"] and all(c.get("kind") != "delegate" for c in own["calls"]), own
subprocess.run(["git", "-C", str(project), "restore", "value.txt"], check=True)
patch_file = project / ".oms/artifacts/test-result.patch"
patch_file.write_text("fixture patch bytes\n")
digest = saved._relative(project, ".oms/artifacts/test-result.patch")["sha256"]
delegated = {"kind": "delegate", "task_id": "test-land", "attempt_id": "att_worker",
             "patch": ".oms/artifacts/test-result.patch", "patch_sha256": digest, "plan_id": "fixture-plan", "exit": 0}
landed = {"kind": "patch-land", "task_id": "test-land", "patch_sha256": digest,
          "plan_id": "fixture-plan", "exit": 0}
assert saved._admission(project, [delegated, landed], "test-land", {"attempt_id": "att_worker"}) == landed
assert saved._admission(project, [delegated, dict(landed, plan_id="other")], "test-land", {"attempt_id": "att_worker"}) is None
patch_file.write_text("changed patch bytes\n")
assert saved._admission(project, [delegated, landed], "test-land", {"attempt_id": "att_worker"}) is None

# A strict full snapshot retains task evidence older than 1000 unrelated rows.
import runpy
index_store = runpy.run_path(str(root / "scripts/lib/artifact-index-store.py"))
index_path = str(project / ".oms/artifacts/index.jsonl")
index_before = index_store["read_index"](str(project), index_path)
try:
    old_event = {"event_id": "evt_before_window", "kind": "call", "task_id": "before-index-window", "attempt_id": "att_before_window", "exit": 0}
    index_store["append_rows"](str(project), index_path,
        [old_event] + [{"event_id": "evt_noise_%s" % n, "kind": "call", "task_id": "noise-%s" % n, "exit": 0} for n in range(1005)],
        keep=5000, high=6000)
    assert old_event in saved._records(project, full=True)[1]
    selected_rows = saved._records(project, full=True, task_id="before-index-window",
                                   _attempts=[{"attempt_id": "att_before_window", "task_id": "before-index-window"}])[1]
    assert old_event in selected_rows and not any(r.get("task_id", "").startswith("noise-") for r in selected_rows)
    retained = saved.results(project, "before-index-window")
    assert retained["rows"][0]["calls"][0]["attempt_id"] == "att_before_window", retained
    assert "all retained artifact events" in retained["coverage"]
    with patch.object(review_settlement, "_parents", return_value={"fixture-parent"}), \
            patch.object(review_settlement, "_settle", return_value={}) as settle:
        review_settlement.settle_review(project)
        assert old_event in settle.call_args.args[2]
    with patch.object(saved.runpy, "run_path", return_value={"read_index": lambda *a, **k: (_ for _ in ()).throw(ValueError("partial"))}):
        try:
            saved._records(project, full=True)
            raise AssertionError("partial index evidence was accepted")
        except ValueError as error:
            assert "coverage incomplete" in str(error), error
finally:
    index_store["replace_bytes"](str(project), index_path, index_before, keep=5000)

# Test retry fencing and durable reopen using the transport boundary alone.
# The production artifact/lifecycle writers still run; no app socket is used.
with patch.dict(os.environ, environment, clear=True):
    unlanded = {"attempt_id": "att_hidden_write", "task_id": "hidden-write", "state": "review",
                "refs": {"panel_role": "worker", "panel_access": "write"}}
    recent = [{"attempt_id": "att_other_%s" % n, "task_id": "other-%s" % n} for n in range(301)]
    def windowed_query(repo, verb, *args):
        if verb == "artifact-index":
            return {"rows": []}
        values = [unlanded] + recent
        return values[-int(args[args.index("--limit") + 1]):] if "--limit" in args else values
    with patch.object(saved, "_query", side_effect=windowed_query):
        try:
            saved.finalize(project, "codex", "hidden-write", str(summary), "accepted", "true", ["value.txt"])
            raise AssertionError("old unlanded write worker escaped acceptance")
        except ValueError as error:
            assert "matching landed patch" in str(error)
    history = ([{"task_id": "history-%s" % n, "ts": "2026-01-%02d" % (n + 1)} for n in range(21)] +
               [{"task_id": "history-0", "ts": "2026-02-01"}])
    with patch.object(saved, "_records", return_value=([], history)):
        assert saved.results(project)["rows"][0]["task_id"] == "history-0"
    mine = [{"attempt_id": "att_old", "task_id": "old-worker", "updated_at": "2026-01-01",
             "refs": {"panel_room_participant": "chat-old"}}]
    noise = [{"attempt_id": "att_new_%s" % n, "task_id": "new-%02d" % n, "updated_at": "2026-02-%02d" % (n + 1),
              "refs": {"panel_room_participant": "chat-new"}} for n in range(25)]
    clash = [{"attempt_id": "att_clash", "task_id": "old-worker", "updated_at": "2026-03-01",
              "refs": {"panel_room_participant": "chat-new"}}]
    old_call = {"kind": "call", "task_id": "old-worker", "attempt_id": "att_old", "exit": 0}
    clash_call = dict(old_call, attempt_id="att_clash", exit=1)
    with patch.object(saved, "_records", return_value=(mine + noise, [old_call])):
        old = saved.results(project, room_participant="chat-old")
        assert [r["task_id"] for r in old["rows"]] == ["old-worker"] and not old["truncated"], old
        assert [call["attempt_id"] for call in old["rows"][0]["calls"]] == ["att_old"]
        recent = saved.results(project, room_participant="chat-new")
        assert [r["task_id"] for r in recent["rows"]] == ["new-%02d" % n for n in range(24, 4, -1)] and recent["truncated"]
        assert saved.results(project, "missing-task")["truncated"]
    with patch.object(saved, "_records", return_value=(mine + noise + clash, [old_call, clash_call])):
        selected = saved.results(project, room_participant="chat-old")
        assert [r["task_id"] for r in selected["rows"]] == ["old-worker"]
        assert [call["attempt_id"] for call in selected["rows"][0]["calls"]] == ["att_old"]
        assert not saved.results(project, "old-worker", "chat-other")["rows"][0]["calls"]
    # A call's scoped results never adopt tasks that aged out of the attempt window, even when
    # their whole-main result names the call's own parent main.
    child = {"attempt_id": "att_child", "task_id": "child-task", "parent_attempt_id": "main-aged",
             "updated_at": "2026-05-01", "refs": {"panel_room_participant": "call-child"}}
    aged_ids = ["aged-%s" % n for n in range(3)]
    aged_rows = [{"kind": "panel-result", "task_id": ident, "ts": "2026-04-%02d" % (n + 1)}
                 for n, ident in enumerate(aged_ids)]
    child_call = {"kind": "call", "task_id": "child-task", "attempt_id": "att_child", "exit": 0}
    with patch.object(saved, "_records", return_value=([child], aged_rows + [child_call])), \
            patch.object(saved, "_load_result", return_value=dict(parent_task, main_attempt_id="main-aged")):
        scoped_rows = saved.results(project, room_participant="call-child")["rows"]
        assert [r["task_id"] for r in scoped_rows] == ["child-task"], scoped_rows
        assert not saved.results(project, room_participant="call-other")["rows"]
        assert saved.results(project, aged_ids[0])["rows"][0]["task_id"] == aged_ids[0]
    preflight_attempt = {"attempt_id": "att_preflight", "task_id": "preflight-check", "state": "done",
                         "updated_at": "2026-04-01", "refs": {}}
    staged_attempt = {"attempt_id": "att_staged", "task_id": "stage-check", "state": "done",
                      "updated_at": "2026-04-02", "refs": {}}
    staged_calls = [dict(kind="delegate", task_id="stage-check", attempt_id="att_staged", exit=0, verify_exit=0),
                    dict(kind="review", task_id="stage-check", attempt_id="att_review", exit=2)]
    with patch.object(saved, "_records", return_value=([preflight_attempt, staged_attempt], staged_calls)):
        default_results = saved.results(project)
        assert [row["task_id"] for row in default_results["rows"]] == ["stage-check"]
        assert {row["task_id"] for row in saved.results(project, include_preflight=True)["rows"]} == {
            "preflight-check", "stage-check"}
        staged = default_results["rows"][0]
        assert staged["stages"]["implement"]["state"] == "passed"
        assert staged["stages"]["verify"]["state"] == "passed"
        assert staged["stages"]["review"]["state"] == "failed"
        assert staged["representative_state"] == "review failed" and staged["next_action"] == "retry review"
        page = saved.results(project, offset=0, limit=1)
        assert page["offset"] == 0 and page["omitted"] == 0 and page["omitted_range"] is None
    exercise_continued_stage_rollup(project)
    review_fixture_repo = temporary / "review-stage-verdict"
    review_fixture_repo.mkdir()

    def review_attempt(task):
        return {"attempt_id": task, "task_id": task, "state": "done", "refs": {}}

    def review_outcome(task, run, exit_code, overall):
        return dict(task_id=task, kind="review-outcome", exit=exit_code,
                    review={"run": run, "overall": overall, "gate_verify_exit": 0})

    passing = review_outcome("review-outcome-pass", "20261023T083700Z-42", 0, 0)
    failing = review_outcome("review-outcome-fail", "20261023T083701Z-43", 1, 1)
    for task, rows, expected in (
            ("review-outcome-pass", [passing], "passed"),
            ("review-outcome-fail", [failing], "failed"),
            ("review-exit-fail", [dict(task_id="review-exit-fail", kind="review", exit=124,
                                        run_id="20261023T083702Z-44")], "failed"),
            ("review-exit-zero", [dict(task_id="review-exit-zero", kind="review", exit=0,
                                        run_id="20261023T083703Z-45")], "inconclusive")):
        with patch.object(saved, "_records", return_value=([review_attempt(task)], rows)):
            projected = saved.results(review_fixture_repo, task_id=task)["rows"][0]
        assert projected["stages"]["review"]["state"] == expected, projected
        if expected == "inconclusive":
            assert projected["representative_state"] == "review inconclusive", projected
        if task == "review-exit-fail":
            assert projected["representative_state"] == "review failed" \
                and projected["next_action"] == "retry review", projected

    latest_task = "review-latest-started"
    latest_rows = [review_outcome(latest_task, "20261023T083800Z-50", 1, 1),
                   review_outcome(latest_task, "20261023T083700Z-49", 0, 0)]
    with patch.object(saved, "_records", return_value=([review_attempt(latest_task)], latest_rows)):
        latest = saved.results(review_fixture_repo, task_id=latest_task)["rows"][0]
    assert latest["stages"]["review"]["state"] == "failed", latest

    boundary_task = "review-outcome-window"
    boundary_rows = [review_outcome(boundary_task, "20261023T083900Z-51", 1, 1)]
    boundary_rows.extend(dict(task_id=boundary_task, kind="ask", attempt_id=boundary_task, exit=0)
                         for _ in range(30))
    with patch.object(saved, "_records", return_value=([review_attempt(boundary_task)], boundary_rows)):
        boundary = saved.results(review_fixture_repo, task_id=boundary_task)["rows"][0]
    assert boundary["stages"]["review"]["state"] == "failed", boundary
    endings = [
        ("cancelled", {"exit": 130, "state": "cancelled", "reason_code": "cancelled_by_owner"},
         "implement cancelled", "cancelled", "none"),
        ("guard", {"exit": 1, "state": "failed", "guard": "scope"},
         "implement guard stop (scope)", "guard stop (scope)", "inspect guard stop and salvage partial work"),
        ("timeout", {"exit": 124, "state": "timed_out"},
         "timed out", "timed out", "salvage (--salvage TASK_ID); --continue if empty, else inspect and re-dispatch with recovered patch"),
        ("verify", {"exit": 0, "verify_exit": 1, "state": "done", "reason_code": "verification_failed"},
         "verify failed", "verify failed", "retry verify"),
        ("provider", {"exit": 1, "state": "failed", "provider_error_exit": 1},
         "implement provider error", "provider error", "retry implement"),
        ("failure", {"exit": 1, "state": "failed"}, "implement failed", "failed", "retry implement"),
    ]
    for suffix, evidence, expected_state, expected_stage_state, expected_action in endings:
        item = dict(task_id="ending-" + suffix, kind="delegate", attempt_id="att-ending-" + suffix, **evidence)
        attempt_evidence = {
            "verify": ("failed", "verification_failed"),
            "timeout": ("failed", "provider_timeout"),
            "guard": ("failed", "worker_authority_violation"),
        }.get(suffix)
        attempts = ([{"attempt_id": item["attempt_id"], "task_id": item["task_id"], "state": attempt_evidence[0],
                      "reason_code": attempt_evidence[1], "updated_at": "2026-10-22", "refs": {}}]
                    if attempt_evidence else [])
        with patch.object(saved, "_records", return_value=(attempts, [item])):
            projected = saved.results(project, task_id="ending-" + suffix)["rows"][0]
        assert projected["representative_state"] == expected_state, projected
        assert projected["next_action"] == expected_action, projected
        if attempt_evidence:
            assert projected["termination_reason"] == "stopped (%s)" % attempt_evidence[1], projected
        stage_name = "verify" if suffix == "verify" else "implement"
        assert projected["stages"][stage_name]["state"] == expected_stage_state, projected
        rendered = question_view.render_results({"rows": [projected]}, width=200)
        assert expected_state in rendered and expected_action in rendered, rendered
    for access in ("read", "write", None, "", "malformed"):
        for with_call in (False, True):
            attempt = {"attempt_id": "att-timeout-access", "task_id": "timeout-access", "state": "timed_out",
                       "reason_code": "provider_timeout", "updated_at": "2026-10-22", "refs": {"panel_access": access}}
            calls = ([dict(task_id="timeout-access", kind="delegate", attempt_id=attempt["attempt_id"],
                           exit=124, state="timed_out")] if with_call else [])
            with patch.object(saved, "_records", return_value=([attempt], calls)):
                row = saved.results(project, task_id="timeout-access")["rows"][0]
            expected = ("continue with --continue TASK_ID" if access == "read" else
                        "salvage (--salvage TASK_ID); --continue if empty, else inspect and re-dispatch with recovered patch")
            assert row["next_action"] == expected, row
    for code, expected, state in (("panel_call_interrupted", "interrupted (caller exited or was signalled)", "cancelled"),
                                  ("provider_failed", "provider failed", "failed"),
                                  ("odd\x1b[31m_code", "stopped (odd?[31m_code)", "cancelled"),
                                  ("heartbeat_expired", None, "blocked")):
        attempt = {"attempt_id": "att-no-artifact", "task_id": "reason-fallback", "state": state,
                   "reason_code": code, "updated_at": "2026-10-22", "refs": {}}
        with patch.object(saved, "_records", return_value=([attempt], [])):
            projected = saved.results(project, task_id="reason-fallback")["rows"][0]
        if state == "blocked":
            assert projected["termination_reason"] is None, projected
            assert projected["representative_state"] == "pending", projected
            assert projected["next_action"] == "record main decision", projected
        else:
            assert projected["termination_reason"] == expected, projected
            assert projected["representative_state"] == expected, projected
            assert projected["next_action"] == ("salvage or re-dispatch" if state == "cancelled"
                                                 else "inspect failure and retry"), projected
        import panel_reader as result_reader
        run_info = result_reader.render_view("Run info", {"rows": [projected]}, {}, width=200, color=False)
        if expected:
            assert "Termination reason: " + expected in run_info, run_info
    accepted_attempt = {"attempt_id": "att-accepted-reason", "task_id": "accepted-reason",
                        "state": "done", "reason_code": "owner_accepted", "updated_at": "2026-10-22", "refs": {}}
    accepted_index = {"task_id": "accepted-reason", "kind": "panel-result", "artifact": "accepted-result.json"}
    accepted_payload = {"outcome": "accepted", "revision": "a" * 24, "summary": "Accepted.", "evidence": []}
    with patch.object(saved, "_records", return_value=([accepted_attempt], [accepted_index])), \
            patch.object(saved, "_load_result", return_value=accepted_payload), \
            patch.object(saved, "_last_delivery", return_value=None):
        accepted_projection = saved.results(project, task_id="accepted-reason")["rows"][0]
    assert accepted_projection["representative_state"] == "main_decision accepted", accepted_projection
    assert accepted_projection["termination_reason"] is None, accepted_projection

    # A final owner decision must be the row's stage/action even if an earlier
    # worker stage failed. The worker verify record also supplies the check line.
    finalized_attempt = {"attempt_id": "att-finalized-view", "task_id": "finalized-view",
                         "state": "done", "updated_at": "2026-10-23", "refs": {}}
    finalized_index = {"task_id": "finalized-view", "kind": "panel-result", "artifact": "finalized-result.json"}
    failed_implement = {"task_id": "finalized-view", "kind": "delegate", "attempt_id": "att-worker-view",
                        "exit": 1, "verify_exit": 0}
    finalized_payload = {"outcome": "completed", "revision": "b" * 24, "summary": "Done.", "evidence": []}
    with patch.object(saved, "_records", return_value=([finalized_attempt], [finalized_index, failed_implement])), \
            patch.object(saved, "_load_result", return_value=finalized_payload), \
            patch.object(saved, "_last_delivery", return_value=None):
        finalized_projection = saved.results(project, task_id="finalized-view")["rows"][0]
    assert finalized_projection["representative_state"] == "main_decision completed", finalized_projection
    assert finalized_projection["next_action"] == "none", finalized_projection
    finalized_text = render_results({"rows": [finalized_projection]}, 200)
    assert "Stage: main_decision completed · Next: none" in finalized_text, finalized_text
    assert "checks passed" in finalized_text and "no checks run" not in finalized_text, finalized_text

    rejected_text = render_results({"rows": [{"task_id": "rejected-view", "outcome": "rejected",
        "verification": {"status": "not_run"}, "stages": {"main_decision": {"state": "rejected"}}}]}, 200)
    assert "Rejected by its main" in rejected_text and "Not reviewed" not in rejected_text, rejected_text
    assert "no checks run" in rejected_text, rejected_text
    failed_checks = render_results({"rows": [{"task_id": "failed-checks", "stages": {
        "verify": {"state": "failed", "exit": 9}}, "calls": [{"verify_exit": 9}]}]}, 200)
    assert "checks failed (exit 9)" in failed_checks, failed_checks
    unattributed_checks = render_results({"rows": [{"task_id": "unattributed-checks", "stages": {
        "verify": {"state": "failed", "exit": 9}}}]}, 200)
    assert "checks failed" in unattributed_checks and "(exit 9)" not in unattributed_checks, unattributed_checks

    def finalized_verify_view(task, verify_calls):
        owner_attempt = {"attempt_id": "att-" + task, "task_id": task,
                         "state": "done", "updated_at": "2026-10-23", "refs": {}}
        result_index = {"task_id": task, "kind": "panel-result", "artifact": task + ".json"}
        payload = {"outcome": "completed", "revision": "c" * 24, "summary": "Done.",
                   "verification": {"status": "not_run"}, "evidence": []}
        records = [result_index] + [dict(task_id=task, kind="delegate", attempt_id="att-%s-%s" % (task, n),
                                         **call) for n, call in enumerate(verify_calls)]
        with patch.object(saved, "_records", return_value=([owner_attempt], records)), \
                patch.object(saved, "_load_result", return_value=payload), \
                patch.object(saved, "_last_delivery", return_value=None):
            row = saved.results(project, task_id=task)["rows"][0]
        return render_results({"rows": [row]}, 200)

    failed_verify_text = finalized_verify_view("final-verify-failed", [
        {"exit": 0, "verify_exit": 9, "verify_status": "failed", "reason_code": "verification_failed"}])
    assert "Completed; the main recorded a summary · checks failed (exit 9)" in failed_verify_text, failed_verify_text
    assert "no checks run" not in failed_verify_text, failed_verify_text
    timed_out_verify_text = finalized_verify_view("final-verify-timeout", [
        {"exit": 0, "verify_exit": 124, "verify_status": "failed", "reason_code": "verification_failed"}])
    assert "Completed; the main recorded a summary · checks timed out" in timed_out_verify_text, timed_out_verify_text
    mixed_verify_text = finalized_verify_view("final-verify-mixed", [
        {"exit": 1, "verify_exit": 9}, {"exit": 0, "verify_exit": 0}])
    assert "checks failed (exit 9)" in mixed_verify_text, mixed_verify_text
    assert "checks failed (exit 0)" not in mixed_verify_text, mixed_verify_text
    # A completed owner result with passed checks must still show a failed durable review.
    review_task = "final-review-failed"
    review_records = [{"task_id": review_task, "kind": "panel-result", "artifact": review_task + ".json"},
                      {"task_id": review_task, "kind": "delegate", "attempt_id": "att-review-worker", "exit": 0, "verify_exit": 0},
                      {"task_id": review_task, "kind": "review-outcome", "exit": 1,
                       "review": {"run": "20261023T083701Z-43", "overall": 1, "gate_verify_exit": 0}}]
    with patch.object(saved, "_records", return_value=([{"attempt_id": "att-" + review_task, "task_id": review_task,
                                                          "state": "done", "updated_at": "2026-10-23", "refs": {}}],
                                                        review_records)), \
            patch.object(saved, "_load_result", return_value={"outcome": "completed", "revision": "d" * 24,
                                                              "summary": "Done.", "evidence": []}), \
            patch.object(saved, "_last_delivery", return_value=None):
        review_row = saved.results(project, task_id=review_task)["rows"][0]
    review_text = render_results({"rows": [review_row]}, 200)
    assert "checks passed" in review_text and "review failed" in review_text, review_text
    unknown_checks_text = render_results({"rows": [{"task_id": "unknown-checks", "stages": {
        "verify": {"state": "unknown"}}}]}, 200)
    assert "checks unknown" in unknown_checks_text and "no checks run" not in unknown_checks_text, unknown_checks_text

    council_attempt = {"attempt_id": "att-council-reason", "task_id": "council-reason",
                       "state": "done", "reason_code": "council_complete", "updated_at": "2026-10-22", "refs": {}}
    with patch.object(saved, "_records", return_value=([council_attempt], [])):
        council_projection = saved.results(project, task_id="council-reason")["rows"][0]
    assert council_projection["termination_reason"] is None, council_projection
    assert council_projection["representative_state"] == "pending", council_projection
    patch_a, patch_b = "a" * 64, "b" * 64
    with patch.object(saved, "_records", return_value=([], [
            {"task_id": "patch-pending", "kind": "delegate", "attempt_id": "attempt-a", "patch": "a.patch", "patch_sha256": patch_a},
            {"task_id": "patch-pending", "kind": "patch-land", "attempt_id": "attempt-a", "patch_sha256": patch_a},
            {"task_id": "patch-pending", "kind": "delegate", "attempt_id": "attempt-b", "patch": "b.patch", "patch_sha256": patch_b}])):
        assert saved.pending_admissions(project, ["patch-pending"]) == {"patch-pending"}
    # The main's own attempt lands the exact bytes; the digest alone settles the patch.
    with patch.object(saved, "_records", return_value=([], [
            {"task_id": "patch-pending", "kind": "delegate", "attempt_id": "attempt-b", "patch": "b.patch", "patch_sha256": patch_b},
            {"task_id": "patch-pending", "kind": "patch-land", "attempt_id": "main-attempt", "patch_sha256": patch_b}])):
        assert saved.pending_admissions(project, ["patch-pending"]) == set()
    # An accept in a clone leaves one canonical panel-accept row: admitted and awaiting land, not pending.
    delegate_b = {"task_id": "patch-pending", "kind": "delegate", "attempt_id": "attempt-b", "patch": "b.patch", "patch_sha256": patch_b}
    accept_b = {"task_id": "patch-pending", "kind": "panel-accept", "attempt_id": "attempt-b", "patch_sha256": patch_b, "ts": "2026-11-02T00:00:00Z"}
    for records, pending, waiting in (
            ([delegate_b], {"patch-pending"}, set()),
            ([delegate_b, accept_b], set(), {"patch-pending"}),
            ([delegate_b, dict(accept_b, attempt_id="attempt-other")], {"patch-pending"}, set()),
            ([delegate_b, dict(accept_b, patch_sha256=patch_a)], {"patch-pending"}, set()),
            ([delegate_b, accept_b, {"task_id": "patch-pending", "kind": "delegate", "attempt_id": "attempt-c", "patch": "c.patch", "patch_sha256": "c" * 64}],
             {"patch-pending"}, set()),
            ([delegate_b, accept_b, {"task_id": "patch-pending", "kind": "patch-land", "patch_sha256": patch_b}], set(), set())):
        with patch.object(saved, "_records", return_value=([], records)):
            assert saved.pending_admissions(project, ["patch-pending"]) == pending, records
            assert saved.admitted_awaiting_land(project, ["patch-pending", "../bad"]) == waiting, records
    newer = {"attempt_id": "attempt-d", "task_id": "patch-pending", "created_at": "2026-11-02T00:00:01Z", "refs": {"panel_access": "write"}}
    for attempt, waiting in ((newer, set()), (dict(newer, created_at="2026-11-02T00:00:00Z"), set()),
                             (dict(newer, created_at="2026-11-01T00:00:00Z"), {"patch-pending"}),
                             (dict(newer, refs={"panel_access": "read"}), {"patch-pending"}),
                             (dict(newer, attempt_id="attempt-b"), {"patch-pending"})):
        with patch.object(saved, "_records", return_value=([attempt], [delegate_b, accept_b])):
            assert saved.admitted_awaiting_land(project, ["patch-pending"]) == waiting, attempt
    paged = [dict(a, parent_attempt_id="mine-main") for a in noise]
    foreign_owner = {"kind": "panel-result", "task_id": "foreign-owner", "ts": "2025-01-01"}
    with patch.object(saved, "_records", return_value=(paged, [foreign_owner])), \
            patch.object(saved, "_load_result", return_value={"main_attempt_id": "foreign-main", "revision": "unused"}):
        twenty_results = saved.results(project, room_participant="chat-new")
        assert len(twenty_results["rows"]) == 20 and twenty_results["truncated"], twenty_results
        assert twenty_results["omitted"] is None and twenty_results["omitted_range"] == [20, None]
        next_page = saved.results(project, room_participant="chat-new", offset=20)
        assert [row["task_id"] for row in next_page["rows"]] == ["new-%02d" % n for n in range(4, -1, -1)]
    uncertain = {"status": "timeout", "persisted": False, "delivery_unknown": True}
    with patch.object(saved.codex_app_notify, "deliver", return_value=uncertain) as transport:
        delivered = saved.finalize(project, "codex", "result-retry", str(summary), "completed", notify=True)
        saved.finalize(project, "codex", "result-retry", str(summary), "completed", notify=True)
        assert transport.call_count == 1
    summary.unlink()
    with patch.object(saved.codex_app_notify, "deliver", return_value={"status": "persisted", "persisted": True}) as transport:
        assert saved.retry_delivery(project, "result-retry")["receipt"]["persisted"]
        assert saved.retry_delivery(project, "result-retry")["receipt"]["persisted"]
        assert transport.call_count == 1
    summary.write_text("A completed result.\n")
    with patch.object(saved.codex_app_notify, "deliver", side_effect=RuntimeError("lost after submission")):
        try:
            saved.finalize(project, "codex", "result-crash", str(summary), "completed", notify=True)
            raise AssertionError("crash fixture did not fire")
        except RuntimeError:
            pass
    with patch.object(saved.codex_app_notify, "deliver") as transport:
        crash = saved.finalize(project, "codex", "result-crash", str(summary), "completed", notify=True)
        assert crash["delivery"]["receipt"]["status"] == "pending" and not transport.called
    for unsafe in ("/private/file", "../value.txt"):
        try:
            saved.finalize(project, "codex", "bad-result", str(summary), "completed", evidence=[unsafe])
            raise AssertionError("unsafe evidence accepted")
        except ValueError:
            pass
    for text in ("/private/absolute", "/etc/passwd", "C:\\private\\file",
                 "".join(("api", "_key", "=", "fixture-value"))):
        summary.write_text(text)
        try:
            saved.finalize(project, "codex", "bad-result", str(summary), "completed")
            raise AssertionError("unsafe summary accepted")
        except ValueError:
            pass
    for text in ("Run ./scripts/check.sh", "Inspect ../x/y", "See https://host/path"):
        summary.write_text(text)
        assert saved.finalize(project, "codex", "relative-summary", str(summary), "completed")["outcome"] == "completed"
    summary.write_text("Safe result.\n")
    link = temporary / "panel-result-linked.txt"
    try:
        link.symlink_to(summary)
        try:
            from contextlib import redirect_stderr
            error_text = io.StringIO()
            with redirect_stderr(error_text):
                saved.finalize(project, "codex", "bad-result", str(link), "completed")
            raise AssertionError("linked summary accepted")
        except ValueError:
            assert "summary file must be a regular non-symlink, non-hard-linked file" in error_text.getvalue(), error_text.getvalue()
    except (OSError, NotImplementedError):
        if os.name != "nt":
            raise
    artifact_dir = review_fixture_repo / ".oms" / "artifacts"
    artifact_dir.mkdir(parents=True, exist_ok=True)
    loop_artifact = artifact_dir / "self-referencing.md"
    sibling_artifact = artifact_dir / "sibling.md"
    sibling_artifact.write_text("## Output\nSibling answer retained\n## Exit\n0\n", encoding="utf-8")
    try:
        loop_artifact.symlink_to(loop_artifact.name)
    except (OSError, NotImplementedError):
        if os.name != "nt":
            raise
    else:
        artifact_attempt = review_attempt("artifact-symlink-loop")
        artifact_attempt["refs"] = {"panel_fallback": "capacity"}
        artifact_rows = [
            dict(task_id="artifact-symlink-loop", kind="ask", attempt_id="artifact-symlink-loop",
                 exit=0, artifact=".oms/artifacts/self-referencing.md", artifact_sha256="0" * 64),
            dict(task_id="artifact-symlink-loop", kind="ask", attempt_id="artifact-symlink-loop",
                 exit=0, artifact=".oms/artifacts/sibling.md",
                 artifact_sha256=hashlib.sha256(sibling_artifact.read_bytes()).hexdigest()),
        ]
        with patch.object(saved, "_records", return_value=([artifact_attempt], artifact_rows)):
            artifact_result = saved.results(review_fixture_repo, task_id="artifact-symlink-loop")["rows"][0]
        assert [call.get("answer") for call in artifact_result["calls"]] == [
            "Answer evidence unavailable", "Sibling answer retained"], artifact_result["calls"]
    def fallback_call(task, body, fallback):
        path = artifact_dir / (task + ".md")
        path.write_text(body, encoding="utf-8")
        attempt = review_attempt(task)
        if fallback:
            attempt["refs"] = {"panel_fallback": "capacity"}
        rows = [dict(task_id=task, kind="ask", attempt_id=task, exit=0, artifact=".oms/artifacts/" + task + ".md",
                     artifact_sha256=hashlib.sha256(path.read_bytes()).hexdigest())]
        with patch.object(saved, "_records", return_value=([attempt], rows)):
            return saved.results(review_fixture_repo, task_id=task)["rows"][0]["calls"][0]["answer"]
    marker = "panel-provider-fallback: provider=claude model=m reason=capacity exit=1\n"
    plain = "## Output\nDefect notes\n```\n" + marker + "```\nTail notes\n## Exit\n0\n"
    kept = fallback_call("fallback-quote", plain, False)
    assert "Defect notes" in kept and "Tail notes" in kept, kept
    fallback_answer = fallback_call(
        "fallback-ran",
        "## Output\nHTTP 429 daily cap exhausted\ntokens used\n12\nusage detail: x\n"
        "panel-provider-fallback: skipped reason=none\n" + marker +
        "FINAL-FALLBACK-REPORT\n" + marker + "QUOTED-LATER\n## Exit\n0\n", True)
    assert "429" in fallback_answer, fallback_answer  # missing sidecar uses combined artifact pipeline (truncated at usage detail)
    # A fallback whose final attempt output carries the body and a quoted marker
    # keeps the body and excludes the primary 429 dump (sidecar present).
    out_path = artifact_dir / "fallback-sidecar.attempt-1.out"
    out_path.write_text("Body kept from final output\n```\n" + marker + "```\n", encoding="utf-8")
    sidecar_path = artifact_dir / "fallback-sidecar.attempts.json"
    sidecar_path.write_text(json.dumps({"attempts": [{"output": "fallback-sidecar.attempt-1.out"}]}),
                             encoding="utf-8")
    artifact_path_sidecar = artifact_dir / "fallback-sidecar.md"
    artifact_path_sidecar.write_text("## Output\nHTTP 429 primary error\n## Exit\n0\n", encoding="utf-8")
    attempt_sidecar = review_attempt("fallback-sidecar")
    attempt_sidecar["refs"] = {"panel_fallback": "capacity"}
    rows_sidecar = [dict(task_id="fallback-sidecar", kind="ask", attempt_id="fallback-sidecar", exit=0,
                          artifact=".oms/artifacts/fallback-sidecar.md",
                          artifact_sha256=hashlib.sha256(artifact_path_sidecar.read_bytes()).hexdigest())]
    with patch.object(saved, "_records", return_value=([attempt_sidecar], rows_sidecar)):
        sidecar_answer = saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"]
    assert "Body kept from final output" in sidecar_answer, sidecar_answer
    assert marker.strip() in sidecar_answer, sidecar_answer
    assert "429" not in sidecar_answer, sidecar_answer
    with patch.object(saved, "_records", return_value=([attempt_sidecar], rows_sidecar)):
        # Requirement 1: a valid final output cannot bypass the indexed digest.
        digest = rows_sidecar[0]["artifact_sha256"]
        rows_sidecar[0]["artifact_sha256"] = "0" * 64
        assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "Answer evidence unavailable"
        rows_sidecar[0]["artifact_sha256"] = digest
        # Requirement 3: malformed entries/fields and empty lists still render.
        for payload in ({"attempts": [None]}, {"attempts": []}, {"attempts": [0, "", {}, {"output": 0}]},
                        {"attempts": [{"output": "../outside.out"}]}, {"attempts": None}, [], 0):
            sidecar_path.write_text(json.dumps(payload), encoding="utf-8")
            assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "HTTP 429 primary error", payload
        sidecar_data = json.dumps({"attempts": [{"output": out_path.name}, None, {"output": 0}]})
        sidecar_path.write_text(sidecar_data, encoding="utf-8")
        assert "Body kept" in saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"]
        # Missing sidecars retain the verified artifact; unreadable records fail closed.
        sidecar_path.unlink()
        assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "HTTP 429 primary error"
        for malformed in ("", "{", "x" * (1024 * 1024 + 1)):
            sidecar_path.write_text(malformed, encoding="utf-8")
            assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "Answer evidence unavailable"
        sidecar_path.write_text(sidecar_data + " " * (1024 * 1024 - len(sidecar_data)), encoding="utf-8")
        assert "Body kept" in saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"]
        sidecar_path.write_text(sidecar_data, encoding="utf-8")
        for size in (0, 1024 * 1024, 1024 * 1024 + 1):
            out_path.write_text("x" * size, encoding="utf-8")
            bounded = saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]
            if size > 1024 * 1024:
                assert bounded["answer"] == "Answer evidence unavailable", bounded
            else:
                assert (bounded["answer"] == "" if size == 0 else
                        bounded["answer"].startswith("x") and len(bounded["answer"]) <= saved.MAX_RESULT), size
                assert bounded["answer_truncated"] == (size > saved.MAX_RESULT), bounded
        out_path.unlink()
        assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "Answer evidence unavailable"
        # Requirements 2 and 4: neither sidecar may follow links, including loops.
        outside = temporary / "outside-attempt.out"
        outside.write_text("OUTSIDE-MUST-NOT-BE-READ", encoding="utf-8")
        for link_path in (out_path, sidecar_path):
            for destination in (outside, link_path):
                if link_path.exists() or link_path.is_symlink():
                    link_path.unlink()
                try:
                    link_path.symlink_to(destination)
                except (OSError, NotImplementedError):
                    if os.name != "nt":
                        raise
                else:
                    with patch.object(saved, "_read", wraps=saved._read) as sidecar_read:
                        assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "Answer evidence unavailable"
                    blocked = link_path.relative_to(review_fixture_repo).as_posix()
                    assert blocked not in [entry.args[1] for entry in sidecar_read.call_args_list]
                finally:
                    if link_path.is_symlink():
                        link_path.unlink()
            link_path.write_text(sidecar_data if link_path == sidecar_path else "Body kept", encoding="utf-8")
        # A nested artifact must use its own adjacent sidecars.
        nested = artifact_dir / "nested"
        nested.mkdir()
        for path in (artifact_path_sidecar, sidecar_path, out_path):
            path.rename(nested / path.name)
        rows_sidecar[0]["artifact"] = ".oms/artifacts/nested/fallback-sidecar.md"
        assert saved.results(review_fixture_repo, task_id="fallback-sidecar")["rows"][0]["calls"][0]["answer"] == "Body kept"
    linked_result = project / saved.results(project, "panel-council-test")["rows"][0]["artifact"]
    linked_result.write_text("changed\n")
    assert saved.results(project, "panel-council-test")["rows"][0]["missing_result"]

for owner, attempt in main_attempts.items():
    call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "transition",
          "--attempt", attempt, "--state", "cancelled"])
closed_parent = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                               "--dispatch", "worker", "--owner", "codex", "--prompt", "Inspect source"],
                              env=dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]),
                              capture_output=True, text=True)
assert closed_parent.returncode != 0 and "identity" in closed_parent.stderr

# The finalize verifier takes a slot of the machine-wide verify pool: with the only slot held it must wait.
import tempfile, threading, time
with tempfile.TemporaryDirectory() as slot_runtime:
    os.environ["XDG_RUNTIME_DIR"], os.environ["OMS_VERIFY_SLOTS"] = slot_runtime, "1"
    inherited_slot = os.environ.pop("OMS_VERIFY_SLOT_HELD", None)  # a verify wrapper's marker would skip the queue
    try:
        holder = saved._hold_verify_slot()
        finished = []
        runner = threading.Thread(target=lambda: finished.append(saved._run_verifier(project, "true", 30)))
        runner.start()
        runner.join(2)
        assert runner.is_alive() and not finished, "finalize verifier ignored the held verify slot"
        saved._release_verify_slot(holder)
        runner.join(int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
        assert finished == [{"exit": 0}], finished
        assert saved._run_verifier(project, 'test -d "$OMS_VERIFY_SLOT_HELD"', 30) == {"exit": 0}, "verifier must see its held slot"
        os.environ["OMS_VERIFY_SLOTS"] = "0"
        try:
            saved._run_verifier(project, "true", 30)
            raise AssertionError("OMS_VERIFY_SLOTS=0 was accepted")
        except ValueError as error:
            assert "OMS_VERIFY_SLOTS" in str(error), error
    finally:
        del os.environ["XDG_RUNTIME_DIR"], os.environ["OMS_VERIFY_SLOTS"]
        if inherited_slot is not None:
            os.environ["OMS_VERIFY_SLOT_HELD"] = inherited_slot

# BEGIN Provider account routing

def exercise_provider_account_routing():
    import provider_accounts as accounts
    import panel_metrics as metrics
    import panel_routing as routing
    import panel_dispatch as dispatch
    from contextlib import redirect_stdout
    from tempfile import TemporaryDirectory

    with TemporaryDirectory(dir=os.environ.get("TMPDIR")) as directory:
        base = Path(directory)
        config_root = base / "config"
        config_path = config_root / "oh-my-setting/provider-accounts.json"
        config_path.parent.mkdir(parents=True)
        homes = [base / name for name in ("credits", "pro")]
        for home in homes:
            home.mkdir()
        value = {"policy": "ordered", "accounts": [
            {"id": home.name, "home": str(home)} for home in homes]}
        now = time.time()

        def configure(data=None, root=None):
            config_path.write_text(json.dumps(root if root is not None else {
                "schema": 1, "providers": {"codex": value if data is None else data}}))

        def quota(index, used, reset=None):
            day = homes[index] / "sessions/2026/11/03"
            day.mkdir(parents=True, exist_ok=True)
            log = day / "rollout-account.jsonl"
            log.write_text(json.dumps({"timestamp": "2026-11-03T00:00:00Z", "payload": {
                "rate_limits": {"primary": {"used_percent": used, "window_minutes": 10080,
                    "resets_at": now + 3600 if reset is None else reset}}}}) + "\n")
            return log

        def choose(model="gpt-6.1-sol", status=None):
            return accounts.select_account("codex", model, readings=status, config=accounts.load_accounts())

        def route():
            return routing.allocate("claude", role="builder", tier="deep", access="write",
                                    installed={"codex"}, repo=base)

        def refused(data, message):
            configure(data)
            try:
                accounts.load_accounts()
            except ValueError as error:
                assert message in str(error), error
            else:
                raise AssertionError("invalid account configuration accepted")

        with patch.dict(os.environ, {"XDG_CONFIG_HOME": str(config_root), "CODEX_HOME": str(homes[0])}):
            legacy = route()
            assert "account" not in legacy
            assert routing.allocate("claude", role="builder", tier="deep", access="write",
                installed={"codex"}, provider_status={"codex": {"weekly_used": 99}}, repo=base) == legacy
            loaded = accounts.load_accounts()
            assert not loaded["configured"] and loaded["providers"]["codex"]["accounts"][0]["id"] == "default"
            assert loaded["providers"]["codex"]["accounts"][0]["home"] == str(homes[0].resolve())
            configure(root={"schema": 1, "providers": {}})
            assert route() == legacy
            assert accounts.select_account("codex", "gpt-6.1-sol")[0] is None
            with patch("panel_chats.current_models", return_value={}), patch("panel_chats.windows", return_value={}):
                assert "accounts" not in metrics.collect({}, now=now)["codex"]
            configure()
            quota(0, 50)
            quota(1, 20)
            assert route()["account"] == "credits"
            for malformed in (None, [], 0):
                assert routing.allocate("claude", role="builder", tier="deep", access="write",
                    installed={"codex"}, provider_status={"codex": malformed}, repo=base)["account"] == "credits"
            assert accounts.select_account("codex", "gpt-6.1-sol", readings=[])[0]["id"] == "credits"
            for used, expected in ((0, "credits"), (89, "credits"), (90, "pro"), (100, "pro"),
                                   (None, "credits"), ("bad", "credits")):
                quota(0, used)
                assert route()["account"] == expected, used
            stale = quota(0, 99)
            os.utime(stale, (now - metrics.CODEX_WEEK_SECONDS - 1,) * 2)
            assert route()["account"] == "credits"
            quota(0, 99, reset=now)
            assert route()["account"] == "credits"
            quota(0, 99)
            quota(1, 99)
            try:
                route()
                raise AssertionError("exhausted sole Codex provider was selected")
            except ValueError as error:
                assert "weekly quota" in str(error), error
            explicit_peer = dict(provider="codex", binary="codex", installed=True, exact_model_override=True,
                                 access=["read", "write"], models=["gpt-6.1-sol"])
            with patch("terminal_panel.provider_catalog", return_value=[explicit_peer]):
                explicit = dispatch.selected_route("claude", "builder", None, "write", None,
                    target="codex", model="gpt-6.1-sol", repo=base)
            assert explicit["account"] == "default" and "explicit" in explicit["account_reason"]
            for usage in (90, 100):
                quota(0, usage)
                quota(1, usage)
                with patch("terminal_panel.provider_catalog", return_value=[explicit_peer]):
                    assert dispatch.selected_route("claude", "builder", None, "write", None,
                        target="codex", model="gpt-6.1-sol", repo=base)["account"] == "default"
            quota(0, 99)
            quota(1, 20)
            chosen = route()
            assert chosen["account"] == "pro"
            fallback = routing.alternate_route(dict(chosen, provider="claude"), {"codex"}, repo=base)
            assert fallback["account"] == "pro"
            deferred = routing.alternate_route(dict(chosen, provider="claude"), {"codex"},
                provider_status={"codex": {"weekly_used": 99}}, repo=base, resolve_accounts=False)
            assert deferred["provider"] == "codex" and "account" not in deferred
            selected, _ = accounts.select_account("codex", "gpt-6.1-sol", readings={
                "credits": {"weekly_used": 99, "resets_at": now}, "pro": {"weekly_used": 50}}, now=now)
            assert selected["id"] == "credits"
            expired = {"credits": {"weekly_used": 99, "resets_at": now - 1},
                       "pro": {"weekly_used": 99, "resets_at": now + 3600}}
            selected_route = routing.allocate("claude", role="builder", tier="deep", access="write",
                installed={"codex", "claude"}, provider_status={"codex": {"accounts": expired},
                    "claude": {"weekly_used": 80}}, repo=base)
            assert selected_route["provider"] == "codex" and selected_route["account"] == "credits"
            readings = metrics.account_readings("codex", now=now)
            assert readings["credits"]["weekly_used"] == 99 and readings["pro"]["weekly_used"] == 20
            with patch.object(metrics.os, "open", side_effect=AssertionError("cached log reread")):
                assert metrics.codex_weekly_reading(now, home=homes[0])["weekly_used"] == 99
                assert metrics.codex_weekly_reading(now, home=homes[1])["weekly_used"] == 20
            with patch("panel_chats.current_models", return_value={}), patch("panel_chats.windows", return_value={}):
                row = metrics.collect({}, now=now)["codex"]
            assert row["weekly_used"] == 99 and row["accounts"] == readings
            value["policy"] = "spread"
            configure()
            quota(0, 50)
            quota(1, 10)
            assert route()["account"] == "pro"
            quota(1, 50)
            assert route()["account"] == "credits"
            worker = {"provider": "codex", "state": "running", "refs": {
                "panel_role": "worker", "panel_account": "credits"}}
            with patch("terminal_panel.events", return_value=json.dumps([worker])):
                assert route()["account"] == "pro"
            for state in ("queued", "starting", "working", "waiting_input", "waiting_approval", "verifying"):
                with patch("terminal_panel.events", return_value=json.dumps([dict(worker, state=state)])):
                    assert route()["account"] == "pro"
            for state in ("done", "failed", "cancelled"):
                with patch("terminal_panel.events", return_value=json.dumps([dict(worker, state=state)])):
                    assert route()["account"] == "credits"
            # Missing readings rank after measured quota; two unknowns still split by running count.
            selected, reason = accounts.select_account("codex", "gpt-6.1-sol", readings={},
                running={"credits": 1}, config=accounts.load_accounts())
            assert selected["id"] == "pro" and "spread" in reason
            value["policy"] = "ordered"
            configure()
            cache = homes[0] / "models_cache.json"
            cache.write_text(json.dumps({"models": [{"slug": "gpt-6.1-sol"}]}))
            selected, reason = choose("gpt-6.1-terra")
            assert selected["id"] == "pro" and "credits" in reason and "cached model list" in reason
            assert choose()[0]["id"] == "credits"
            for body in ("{", "null", '{"models":null}', '{"models":[{}]}'):
                cache.write_text(body)
                assert choose()[0]["id"] == "credits"
            cache.write_text('{"models":[]}')
            assert choose()[0]["id"] == "pro"
            (homes[1] / "models_cache.json").write_text('{"models":[]}')
            assert choose()[0] is None
            with patch("terminal_panel.provider_catalog", return_value=[explicit_peer]):
                assert dispatch.selected_route("claude", "builder", None, "write", None,
                    target="codex", model="gpt-6.1-sol", repo=base)["account"] == "default"
            assert routing.alternate_route(dict(chosen, provider="claude"), {"codex"}, repo=base) is None
            try:
                routing.resolve_provider_account(legacy, repo=base)
                raise AssertionError("unsupported model account selected")
            except ValueError as error:
                assert "cached model list" in str(error)
            (homes[1] / "models_cache.json").unlink()
            cache.write_bytes(b"")
            assert choose()[0]["id"] == "credits"
            payload = json.dumps({"models": [{"slug": "gpt-6.1-sol"}]}).encode()
            cache.write_bytes(payload + b" " * (4 * 1024 * 1024 - len(payload)))
            assert accounts.model_slugs(homes[0]) == {"gpt-6.1-sol"}
            with cache.open("ab") as stream:
                stream.write(b" ")
            assert accounts.model_slugs(homes[0]) is None
            with patch.object(accounts.os, "open", side_effect=PermissionError):
                assert accounts.model_slugs(homes[0]) is None
            cache.unlink()
            # Tier config remains provider-only; runtime account fields cannot enter it.
            assert not routing.valid_route(dict(provider="codex", model="gpt-6.1-sol", effort="high", account="credits"))
            peer = dict(provider="codex", binary="codex", installed=True, exact_model_override=True,
                        access=["read", "write"], models=["gpt-6.1-sol"])
            with patch("terminal_panel.provider_catalog", return_value=[peer]):
                selected = dispatch.selected_route("claude", "builder", None, "write", None,
                                                   target="codex", model="gpt-6.1-sol", repo=base)
            assert selected["account"] == "credits"
            cache.write_text(json.dumps({"models": [{"slug": "gpt-6-luna"}]}))
            with patch("terminal_panel.provider_catalog", return_value=[peer]):
                selected = dispatch.selected_route("claude", "builder", "deep", "write", None, repo=base)
            assert selected["account"] == "pro" and "cached model list" in selected["account_reason"]
            cache.write_text(json.dumps({"models": [{"slug": "gpt-6.2-sol"}]}))
            (homes[1] / "models_cache.json").write_text('{"models":[]}')
            next_peer = dict(peer, models=["gpt-6.2-sol"])
            claude = dict(legacy, provider="claude", model="claude-opus-5-5", access="read", purpose="explain")
            with patch("terminal_panel.selected_route", return_value=claude), \
                    patch("terminal_panel.child_environment", return_value={}), \
                    patch("terminal_panel.routing_status", return_value={}), \
                    patch("terminal_panel.provider_catalog", return_value=[next_peer]), \
                    patch("terminal_panel.run_dispatch", side_effect=lambda *args, **kwargs: args[-2]):
                alternate = dispatch.dispatch(project, "claude", "builder", "deep", "read", None, prompt="inspect")
            assert alternate["model"] == "gpt-6.2-sol" and alternate["account"] == "credits"
            (homes[1] / "models_cache.json").unlink()
            cache.unlink()
            # The existing launch anchor carries the decision without storing its home.
            member = "call-accounts-" + uuid.uuid4().hex[:12]
            call_record = dispatch._prepare_dispatch_call(project, "account-room", member, "main", route=selected)
            anchor = dispatch.dispatch_store.read(project, member + "-launch.json")
            assert anchor["route"]["account"] == "pro" and "home" not in anchor["route"]
            assert "account_applied" not in anchor["route"]
            assert dispatch._dispatch_call_record(project, "account-room", {"participant": member, "parent": "main"}) == call_record
            live = dict(worker, state="working", refs={"panel_role": "worker", "panel_room_participant": member})
            with patch("terminal_panel.events", return_value=json.dumps([live])):
                assert routing.running_accounts("codex", project) == {"pro": 1}
            try:
                dispatch._prepare_dispatch_call(project, "account-room", member, "main", route=selected)
                raise AssertionError("a second writer replaced the launch")
            except (OSError, ValueError):
                pass
            interrupted = "call-accounts-" + uuid.uuid4().hex[:12]
            with patch.object(dispatch.dispatch_store, "write", side_effect=ValueError("interrupted")):
                try:
                    dispatch._prepare_dispatch_call(project, "account-room", interrupted, "main", route=selected)
                    raise AssertionError("interrupted launch succeeded")
                except ValueError:
                    pass
            assert dispatch._dispatch_call_record(project, "account-room", {"participant": interrupted, "parent": "main"}) is None
            stream = io.StringIO()
            with redirect_stdout(stream):
                dispatch.run_dispatch(base, "claude", "builder", "read", "inspect", None, None, "accounts",
                    True, None, dict(legacy, account="credits", access="read", purpose="explain"), None, None, None)
            assert json.loads(stream.getvalue())["route"]["account"] == "credits"
            assert dispatch.route_label(dict(provider="codex", account="credits")) == "codex account=credits (runs under its own home)"
            assert dispatch.route_label({"provider": "codex"}) == "codex"
            configure(dict(value, policy="spread"))
            selected, _ = choose(status={"credits": {"weekly_used": 0}, "pro": {"weekly_used": None}})
            assert selected["id"] == "credits"
            without_policy = dict(value)
            without_policy.pop("policy")
            configure(without_policy)
            assert accounts.load_accounts()["providers"]["codex"]["policy"] == "ordered"
            for data, message in (
                (dict(value, extra=True), "unknown"),
                (dict(value, policy=""), "policy"),
                (dict(value, policy="invalid"), "policy"),
                (dict(value, policy=0), "policy"),
                (dict(value, accounts=[]), "non-empty"),
                (dict(value, accounts=None), "non-empty"),
                (dict(value, accounts=0), "non-empty"),
                (dict(value, accounts=[dict(value["accounts"][0], extra=True)]), "unknown"),
                (dict(value, accounts=[dict(value["accounts"][0], kind="other")]), "unknown"),
                (dict(value, accounts=[dict(value["accounts"][0], home="relative")]), "absolute"),
                (dict(value, accounts=[dict(value["accounts"][0], home="")]), "absolute"),
                (dict(value, accounts=[dict(value["accounts"][0], home=None)]), "absolute"),
                (dict(value, accounts=[dict(value["accounts"][0], id="")]), "id"),
                (dict(value, accounts=[dict(value["accounts"][0], id=0)]), "id"),
                (dict(value, accounts=[dict(value["accounts"][0], id="../bad")]), "id"),
                (dict(value, accounts=[value["accounts"][0]] * 2), "duplicate"),
                (dict(value, accounts=[value["accounts"][0], dict(value["accounts"][1], home=str(homes[0]))]), "duplicate")):
                refused(data, message)
            for root, message in (
                ({"schema": True, "providers": {}}, "schema"),
                ({"schema": 0, "providers": {}}, "schema"),
                ({}, "schema"),
                ([], "unknown"),
                ({"schema": 1, "providers": {}, "extra": 1}, "unknown"),
                ({"schema": 1, "providers": []}, "providers"),
                ({"schema": 1, "providers": 0}, "providers"),
                ({"schema": 1, "providers": None}, "providers"),
                ({"schema": 1, "providers": {"codex": {}}}, "non-empty"),
                ({"schema": 1, "providers": {"codex": []}}, "unknown"),
                ({"schema": 1, "providers": {"unknown": value}}, "unknown provider"),
                ({"schema": 1, "providers": {"claude": value}}, "accounts for claude are not supported yet"),
                ({"schema": 1, "providers": {"pi": value}}, "accounts for pi are not supported yet")):
                configure(root=root)
                try:
                    accounts.load_accounts()
                    raise AssertionError("invalid provider accounts accepted")
                except ValueError as error:
                    assert message in str(error), error
            # The global uniqueness rule also applies once another provider is enabled.
            enabled = dict(accounts.PROVIDER_HOMES, claude=("CLAUDE_CONFIG_DIR", True))
            with patch.object(accounts, "PROVIDER_HOMES", enabled):
                for entry in (value["accounts"][0], dict(value["accounts"][1], id="credits"),
                              dict(value["accounts"][0], id="other")):
                    configure(root={"schema": 1, "providers": {
                        "codex": {"accounts": [value["accounts"][0]]}, "claude": {"accounts": [entry]}}})
                    try:
                        accounts.load_accounts()
                        raise AssertionError("cross-provider duplicate accepted")
                    except ValueError as error:
                        assert "duplicate" in str(error), error
                configure(root={"schema": 1, "providers": {
                    "codex": {"accounts": [value["accounts"][0]]},
                    "claude": {"accounts": [value["accounts"][1]]}}})
                assert set(accounts.load_accounts()["providers"]) == {"codex", "claude"}
            configure(dict(value, accounts=[dict(value["accounts"][0], id="x" * 64)]))
            assert accounts.load_accounts()["providers"]["codex"]["accounts"][0]["id"] == "x" * 64
            refused(dict(value, accounts=[dict(value["accounts"][0], id="x" * 65)]), "id")
            alias = base / "alias"
            try:
                alias.symlink_to(homes[0], target_is_directory=True)
            except OSError:
                pass
            else:
                refused(dict(value, accounts=[value["accounts"][0], dict(value["accounts"][1], home=str(alias))]), "duplicate")
            # Relative XDG roots are ignored, including when the relative path exists.
            fallback_home = base / "fallback-home"
            fallback_config = fallback_home / ".config/oh-my-setting/provider-accounts.json"
            fallback_config.parent.mkdir(parents=True)
            fallback_config.write_text(json.dumps({"schema": 1, "providers": {"codex": value}}))
            with patch.dict(os.environ, {"XDG_CONFIG_HOME": "relative"}), \
                    patch.object(accounts.Path, "home", return_value=fallback_home):
                assert accounts.load_accounts()["configured"]
            try:
                accounts.load_accounts("relative")
                raise AssertionError("relative explicit config path accepted")
            except ValueError as error:
                assert "absolute" in str(error)
            configure(dict(value, accounts=[dict(value["accounts"][0], home=str(config_path))]))
            bad_home = accounts.load_accounts()["providers"]["codex"]["accounts"][0]
            assert "directory" in bad_home["error"]
            config_path.write_text("{")
            try:
                accounts.load_accounts()
                raise AssertionError("malformed configuration accepted")
            except ValueError as error:
                assert "invalid provider accounts configuration" in str(error)
            with patch("panel_chats.current_models", return_value={}), patch("panel_chats.windows", return_value={}):
                status = metrics.collect({}, now=now)
            assert "invalid provider accounts configuration" in status["codex"]["accounts_error"]
            assert "Claude" in metrics.usage_line({"provider_status": status})
            assert "accounts:" in metrics.usage_line({"provider_status": status})
            assert routing.allocate("codex", role="builder", tier="deep", access="write",
                installed={"claude"}, repo=base)["provider"] == "claude"
            assert routing.allocate("codex", role="builder", tier="deep", access="write",
                installed={"claude", "codex"}, repo=base)["provider"] == "claude"
            try:
                route()
                raise AssertionError("invalid config allowed a Codex route")
            except ValueError as error:
                assert "invalid provider accounts configuration" in str(error)
            assert routing.resolve_provider_account({"provider": "claude"}) == {"provider": "claude"}
            assert routing.alternate_route(dict(legacy, provider="pi"), {"claude"}, repo=base)["provider"] == "claude"
            assert routing.alternate_route(dict(legacy, provider="claude"), {"codex"}, repo=base) is None
            configure()
            shutil.rmtree(homes[0])
            with patch("panel_chats.current_models", return_value={}), patch("panel_chats.windows", return_value={}):
                status = metrics.collect({}, now=now)
            assert "credits" in status["codex"]["accounts_error"]
            assert "directory" in status["codex"]["accounts"]["credits"]["error"]
            assert "credits" in metrics.usage_line({"provider_status": status})
            assert route()["account"] == "pro"
            assert routing.allocate("codex", role="builder", tier="deep", access="write",
                installed={"claude"}, repo=base)["provider"] == "claude"
            shutil.rmtree(homes[1])
            assert accounts.select_account("codex", "gpt-6.1-sol")[0] is None
            try:
                route()
                raise AssertionError("missing account homes were selected")
            except ValueError as error:
                assert "directory" in str(error), error
            config_path.write_text('{"schema":1,"schema":1}')
            try:
                accounts.load_accounts()
                raise AssertionError("duplicate JSON keys accepted")
            except ValueError as error:
                assert "duplicate" in str(error)

exercise_provider_account_routing()
# END Provider account routing

# BEGIN Provider account execution
def exercise_provider_account_execution():
    import provider_accounts as accounts
    import panel_dispatch as dispatch
    import panel_routing as routing
    import io
    from contextlib import redirect_stdout, redirect_stderr
    from tempfile import TemporaryDirectory

    with TemporaryDirectory(dir=temporary) as directory:
        base = Path(directory).resolve()
        assert project.resolve() not in (base, *base.parents)
        homes = [base / name for name in ("credits", "pro", "third")]
        for number, account_home in enumerate(homes):
            account_home.mkdir()
            models = ["gpt-6.1-sol"] + (["gpt-6.1-terra"] if number else [])
            (account_home / "models_cache.json").write_text(json.dumps({"models": [{"slug": model} for model in models]}))
        config_root = base / "config"
        config = config_root / "oh-my-setting/provider-accounts.json"
        config.parent.mkdir(parents=True)
        value = {"schema": 1, "providers": {"codex": {"accounts": [
            {"id": account_home.name, "home": str(account_home)} for account_home in homes]}}}
        seen = base / "homes.jsonl"
        standin = base / "bin"
        standin.mkdir()
        executable = standin / "codex"
        executable.write_text("#!/usr/bin/env python3\n" + '''import json, os, sys
from pathlib import Path
if os.environ.get("OMS_HARNESS_CHILD") == "1":
    if os.environ.get("OMS_PANEL_ROLE") != "worker":
        print("Answer: inspected the bounded repository task.\\nGATE: pass")
        sys.exit(0)
    home = os.environ.get("CODEX_HOME")
    provider = Path(sys.argv[0]).name
    with open(os.environ["ACCOUNT_TEST_SEEN"], "a") as stream:
        stream.write(json.dumps({"home": home, "provider": provider, "argv": sys.argv[1:]}) + "\\n")
    mode = os.environ.get("ACCOUNT_TEST_MODE")
    if provider == "claude" and mode not in ("down", "unavailable", "all-to-provider", "fresh-provider"):
        print("Error: HTTP 503 service unavailable")
        sys.exit(1)
    if provider == "codex" and mode in ("down", "unavailable"):
        print("Error: HTTP 503 service unavailable" if mode == "down" else "Error: model not found")
        sys.exit(1)
    if provider == "codex" and (mode in ("all", "all-to-provider") or mode in ("capacity", "dirty", "policy") and Path(home).name == "credits"):
        if mode == "dirty":
            with open("value.txt", "a") as stream:
                stream.write("changed before capacity failure\\n")
        print("Error: usage limit; blocked by content filtering" if mode == "policy" else "Error: You've hit your usage limit. Try again later.")
        sys.exit(1)
    session = os.environ.get("ACCOUNT_TEST_SESSION")
    if not session and provider == "codex" and "resume" in sys.argv:
        resume_args = sys.argv[sys.argv.index("resume") + 1:]
        session = resume_args[1] if resume_args[0] == "--all" else resume_args[0]
    if session:
        with open("value.txt", "a") as stream:
            stream.write("session fixture change\\n")
        folder = Path(home) / "sessions/2026/11/03"
        folder.mkdir(parents=True, exist_ok=True)
        (folder / ("rollout-" + session + ".jsonl")).write_text("{}\\n")
        os.environ["PANEL_TEST_SESSION"] = session
os.execv(os.environ["ACCOUNT_TEST_ORIGINAL"], [os.environ["ACCOUNT_TEST_ORIGINAL"], *sys.argv[1:]])
''')
        executable.chmod(0o755)
        other = standin / "claude"
        other.write_text(executable.read_text())
        other.chmod(0o755)
        ident = room.create(project, "account-execution", "Account execution fixture")
        main = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                     "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                     "--ref", "panel_room_id=" + ident, "--then", "starting", "--then", "working"]).strip()
        legacy_main_join(project, ident, main, "claude", model="claude-opus-5-5")
        env = dict(environment, OMS_ROOM_ID=ident, OMS_ROOM_REPO=str(project), OMS_ROOM_PARTICIPANT=main,
                   OMS_PANEL_MAIN_ATTEMPT=main, XDG_CONFIG_HOME=str(config_root), ACCOUNT_TEST_SEEN=str(seen),
                   ACCOUNT_TEST_ORIGINAL=str(binary / "codex"), PATH=str(standin) + os.pathsep + environment["PATH"])
        before_review = (project / "value.txt").read_bytes()
        read_result = subprocess.run([str(executable), "exec", "--sandbox", "read-only"], cwd=str(project),
            env=dict(env, OMS_HARNESS_CHILD="1", OMS_PANEL_ROLE="reviewer", ACCOUNT_TEST_SESSION=str(uuid.uuid4())),
            capture_output=True, text=True, timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
        assert read_result.returncode == 0, read_result.stdout + read_result.stderr
        assert (project / "value.txt").read_bytes() == before_review and not seen.exists(), "review fixture wrote state"
        request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "builder",
                   "--owner", "claude", "--access", "write", "--to", "codex", "--model", "gpt-6.1-sol",
                   "--brief-file", str(brief), "--verify", "true"]

        def run(name, extra=None, model=None, verify=None, continuing=False, route_args=None,
                legacy_account=False, missing_anchor=None, writable_home=None):
            argv = list(request)
            if continuing or route_args:
                start = argv.index("--to")
                del argv[start:start + 4]
            if model:
                argv[argv.index("--model") + 1] = model
            if verify:
                argv[argv.index("--verify") + 1] = verify
            argv += route_args or []
            argv += ["--continue" if continuing else "--task-id", name]
            before = len(seen.read_text().splitlines()) if seen.exists() else 0
            call_env = dict(env, **(extra or {}))
            if legacy_account or missing_anchor or writable_home:
                code = "import sys, terminal_panel\n"
                if legacy_account:
                    code += "import provider_accounts\nprovider_accounts.route_account_home = lambda *args, **kwargs: None\n"
                if missing_anchor:
                    code += ("import dispatch_supervisor as store\noriginal = store.read\n"
                             "def read(repo, name):\n"
                             "    if name == %r: raise FileNotFoundError('fixture anchor unavailable')\n"
                             "    return original(repo, name)\nstore.read = read\n" % (missing_anchor + "-launch.json"))
                if writable_home:
                    code += ("from pathlib import Path\nimport provider_accounts\n"
                             "original_home = provider_accounts.route_account_home\n"
                             "def home(route):\n"
                             "    Path(%r).chmod(0o777)\n    return original_home(route)\n"
                             "provider_accounts.route_account_home = home\n" % str(writable_home))
                code += "sys.exit(terminal_panel.main(sys.argv[1:]))\n"
                argv = [sys.executable, "-c", code, *argv[3:]]
                call_env["PYTHONPATH"] = str(root / "scripts/lib")
            result = subprocess.run(argv, env=call_env, cwd=str(project),
                                    capture_output=True, text=True, timeout=int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")))
            rows = [json.loads(line) for line in seen.read_text().splitlines()][before:] if seen.exists() else []
            return result, rows

        def launched(name, expected, **kwargs):
            result, rows = run(name, **kwargs)
            if result.returncode not in (0, 6):
                with patch.dict(os.environ, env):
                    attempts, indexed = panel_results._records(project, full=True, task_id=name)
                raise AssertionError((result.stdout + result.stderr,
                    [row for row in attempts if row.get("task_id") == name], indexed))
            assert [row["home"] for row in rows] == expected, (rows, result.stdout, result.stderr)
            return rows

        def owner_env(label):
            ident = room.create(project, label, "Account fallback and continuation")
            main = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                "--ref", "panel_room_id=" + ident, "--then", "starting", "--then", "working"]).strip()
            legacy_main_join(project, ident, main, "claude", model="claude-opus-5-5")
            return dict(env, OMS_ROOM_ID=ident, OMS_ROOM_PARTICIPANT=main, OMS_PANEL_MAIN_ATTEMPT=main)

        def latest_member(name):
            attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"], env))
            return next(row["refs"]["panel_room_participant"] for row in reversed(attempts)
                        if row.get("task_id") == name and row.get("refs", {}).get("panel_role") == "worker")

        # Separate owners keep successful continuation fixtures outside the earlier cases' review backlog.
        saved_env = env
        env = owner_env("account-legacy")
        config.write_text(json.dumps(value))
        regressions = []
        legacy_session = str(uuid.uuid4())
        launched("account-legacy", [env["CODEX_HOME"]], legacy_account=True,
                 extra={"ACCOUNT_TEST_SESSION": legacy_session})
        with patch.dict(os.environ, env):
            anchor = dispatch.dispatch_store.read(project, latest_member("account-legacy") + "-launch.json")
        assert anchor["route"]["account"] == "credits" and "account_applied" not in anchor["route"], anchor
        value["providers"]["codex"]["accounts"].pop(0)
        config.write_text(json.dumps(value))
        for missing in (False, True):
            result, rows = run("account-legacy", continuing=True,
                route_args=["--to", "codex", "--model", "gpt-6.1-sol"],
                missing_anchor=latest_member("account-legacy") if missing else None)
            if (result.returncode not in (0, 6) or [row["home"] for row in rows] != [env["CODEX_HOME"]]
                    or not rows or legacy_session not in rows[0]["argv"]):
                regressions.append(("missing anchor" if missing else "selection-only anchor", result.returncode, rows, result.stderr))
        value["providers"]["codex"]["accounts"].insert(0, {"id": "credits", "home": str(homes[0])})
        config.write_text(json.dumps(value))
        env = owner_env("account-regressions")
        if hasattr(os, "getuid"):
            homes[0].chmod(0o700)
            try:
                result, rows = run("account-insecure-home", writable_home=homes[0])
                if (result.returncode in (0, 6) or rows or "credits" not in result.stderr
                        or "chmod go-w" not in result.stderr):
                    regressions.append(("writable home launch", result.returncode, rows, result.stderr))
            finally:
                homes[0].chmod(0o700)
            launched("account-private-home", [str(homes[0])])
        original_entries = list(value["providers"]["codex"]["accounts"])
        value["providers"]["codex"]["accounts"] = original_entries[:1]
        config.write_text(json.dumps(value))
        for unset in (False, True):
            inherited_home = env.pop("CODEX_HOME") if unset else env["CODEX_HOME"]
            result, rows = run("account-single-fallback-" + str(unset), route_args=["--workload", "deep"],
                               extra={"ACCOUNT_TEST_MODE": "down"})
            if (result.returncode not in (0, 6) or [row["provider"] for row in rows] != ["codex", "claude"]
                    or rows[-1]["home"] != env.get("CODEX_HOME")):
                regressions.append(("single-account fallback environment", result.returncode, rows, result.stderr))
            if unset:
                env["CODEX_HOME"] = inherited_home
        value["providers"]["codex"]["accounts"] = original_entries
        config.write_text(json.dumps(value))
        for mode, expected in (("down", ["codex", "claude"]), ("unavailable", ["codex", "claude"]),
                               ("all-to-provider", ["codex", "codex", "claude"])):
            result, rows = run("account-tier-" + mode, route_args=["--workload", "deep"],
                               extra={"ACCOUNT_TEST_MODE": mode})
            if (result.returncode not in (0, 6) or [row["provider"] for row in rows] != expected
                    or any(row["home"] != env.get("CODEX_HOME") for row in rows if row["provider"] != "codex")):
                regressions.append((mode, result.returncode, rows, result.stderr))
            attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"], env))
            first = next(row for row in attempts if row.get("task_id") == "account-tier-" + mode)
            if first["refs"].get("panel_fallback_decision") != "deferred":
                regressions.append((mode, "incorrect deferred decision", first["refs"].get("panel_fallback_decision")))
        Path(env["CODEX_HOME"]).mkdir(parents=True, exist_ok=True)
        for account_home in homes:
            (account_home / "models_cache.json").write_text('{"models":[]}')
        default_session = str(uuid.uuid4())
        launched("account-default", [env["CODEX_HOME"]], extra={"ACCOUNT_TEST_SESSION": default_session})
        for number, account_home in enumerate(homes):
            models = ["gpt-6.1-sol"] + (["gpt-6.1-terra"] if number else [])
            (account_home / "models_cache.json").write_text(json.dumps({"models": [{"slug": model} for model in models]}))
        for name in ("account-switch-explicit", "account-switch-workload"):
            launched(name, [str(homes[0])], extra={"ACCOUNT_TEST_SESSION": str(uuid.uuid4())})
        value["providers"]["codex"]["accounts"].pop(0)
        config.write_text(json.dumps(value))
        result, rows = run("account-switch-explicit", continuing=True,
                           route_args=["--to", "codex", "--model", "gpt-6.1-sol"])
        if (result.returncode == 0 or rows or "non-Codex provider" not in result.stderr
                or "routed to another provider" not in result.stderr):
            regressions.append(("same-provider refusal recovery", result.returncode, rows, result.stderr))
        for name, options in (("account-switch-explicit", ["--to", "claude", "--model", "claude-opus-5-5"]),
                              ("account-switch-workload", ["--workload", "frontier"])):
            result, rows = run(name, continuing=True, route_args=options, extra={"ACCOUNT_TEST_MODE": "fresh-provider"})
            if (result.returncode not in (0, 6) or [row["provider"] for row in rows] != ["claude"]
                    or "earlier worker ran on codex, not claude" not in result.stderr):
                regressions.append((name, result.returncode, rows, result.stderr))
        result, rows = run("account-default", continuing=True)
        if (result.returncode not in (0, 6) or [row["home"] for row in rows] != [env["CODEX_HOME"]]
                or not rows or default_session not in rows[0]["argv"]):
            regressions.append(("default continuation", result.returncode, rows, result.stderr))
        value["providers"]["codex"]["accounts"].insert(0, {"id": "credits", "home": str(homes[0])})
        config.write_text(json.dumps(value))
        env = saved_env
        assert not regressions, regressions

        # The absent configuration and an empty provider map preserve the caller's home.
        config.unlink()
        launched("account-no-config", [env["CODEX_HOME"]])
        inherited_home = env.pop("CODEX_HOME")
        launched("account-no-home-variable", [None])
        env["CODEX_HOME"] = inherited_home
        config.write_text(json.dumps({"schema": 1, "providers": {}}))
        launched("account-no-entry", [env["CODEX_HOME"]])
        config.write_text(json.dumps(value))
        launched("account-model", [str(homes[1])], model="gpt-6.1-terra")
        # A fallback into Codex gets its own launch environment instead of inheriting the failed provider's.
        before = len(seen.read_text().splitlines())
        with patch.dict(os.environ, env), redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            primary = routing.allocate("codex", role="builder", tier="deep", access="write", installed={"claude"})
            fallback = dict(primary, provider="codex", binary="codex", model="gpt-6.1-sol", account="credits")
            result = dispatch.run_dispatch(project, "claude", "builder", "write", None, str(brief), "true",
                "account-provider-fallback", False, None, primary, None, None, [], fallback=fallback)
        rows = [json.loads(line) for line in seen.read_text().splitlines()][before:]
        assert result == 6 and [row["home"] for row in rows] == [env["CODEX_HOME"], str(homes[0])], (result, rows)
        attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"], env))
        first = next(row for row in attempts if row.get("task_id") == "account-provider-fallback")
        assert first["refs"].get("panel_fallback_decision") == "deferred", first
        before = len(seen.read_text().splitlines())
        with patch.dict(os.environ, dict(env, ACCOUNT_TEST_MODE="down")), \
                redirect_stdout(io.StringIO()), redirect_stderr(io.StringIO()):
            primary = routing.allocate("claude", role="builder", tier="deep", access="write", installed={"codex"})
            unbound = {key: value for key, value in primary.items() if key not in ("account", "account_reason")}
            result = dispatch.run_dispatch(project, "claude", "builder", "write", None, str(brief), "true",
                "account-unbound-codex-fallback", False, None, primary, None, None, [], fallback=unbound,
                account_overflow=False)
        rows = [json.loads(line) for line in seen.read_text().splitlines()][before:]
        assert result != 0 and [row["home"] for row in rows] == [str(homes[0]), env["CODEX_HOME"]], (result, rows)
        launched("account-overflow", [str(homes[0]), str(homes[1])], extra={"ACCOUNT_TEST_MODE": "capacity"})
        attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"], env))
        overflow = [row for row in attempts if row.get("task_id") == "account-overflow"]
        with patch.dict(os.environ, env):
            anchors = [dispatch.dispatch_store.read(project, row["refs"]["panel_room_participant"] + "-launch.json") for row in overflow]
        assert [anchor["route"]["account"] for anchor in anchors] == ["credits", "pro"], anchors
        assert all("home" not in anchor["route"] for anchor in anchors)
        assert all(anchor["route"].get("account_applied") is True for anchor in anchors)
        for mode, expected in (("all", homes[:2]), ("dirty", homes[:1]), ("policy", homes[:1])):
            result, rows = run("account-stop-" + mode, extra={"ACCOUNT_TEST_MODE": mode})
            assert result.returncode != 0 and [row["home"] for row in rows] == [str(home) for home in expected], (mode, result.stdout, result.stderr, rows)
        session = str(uuid.uuid4())
        launched("account-ordered", [str(homes[0])], extra={"ACCOUNT_TEST_SESSION": session})
        # Fresh quota/model selection would choose another account, but native resume keeps its home.
        (homes[0] / "models_cache.json").write_text('{"models":[]}')
        rows = launched("account-ordered", [str(homes[0])], continuing=True)
        assert session in rows[0]["argv"] and "resume" in rows[0]["argv"], rows
        attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"], env))
        resumed = [row for row in attempts if row.get("task_id") == "account-ordered"
                   and row.get("refs", {}).get("panel_role") == "worker"]
        assert resumed[-1]["refs"].get("native_session") == session, resumed
        value["providers"]["codex"]["accounts"] = value["providers"]["codex"]["accounts"][1:]
        config.write_text(json.dumps(value))
        result, rows = run("account-ordered", continuing=True)
        assert result.returncode != 0 and all(part in result.stderr for part in (
            "credits", "configured", "re-register", "--to", "--model", "--workload")) and not rows, result.stdout + result.stderr
        value["providers"]["codex"]["accounts"].insert(0, {"id": "credits", "home": str(homes[0])})
        config.write_text(json.dumps(value))
        (homes[0] / "models_cache.json").write_text('{"models":[{"slug":"gpt-6.1-sol"}]}')
        result, rows = run("account-verify", verify="exit 7", extra={"PANEL_TEST_MODE": "write"})
        assert result.returncode != 0 and [row["home"] for row in rows] == [str(homes[0])], result.stdout + result.stderr
        # Recheck after selection; missing homes and removed ids fail before any child is launched.
        route = {"provider": "codex", "account": "credits", "model": "gpt-6.1-sol"}
        with patch.dict(os.environ, env):
            assert accounts.route_account_home(route) == homes[0]
        shutil.rmtree(homes[0])
        result, rows = run("account-ordered", continuing=True)
        assert result.returncode != 0 and "credits" in result.stderr and "directory" in result.stderr and not rows, result.stdout + result.stderr
        assert accounts.route_account_home({"provider": "claude", "account": "credits"}) is None
        assert accounts.route_account_home({"provider": "codex"}) is None

        configured = accounts.load_accounts(config)
        candidate = dict(route, account="pro")
        with patch.object(accounts, "load_accounts", return_value=configured):
            assert accounts.next_account_route(candidate, readings={})["account"] == "third"
            for ident in ("third", "removed", "", None):
                assert accounts.next_account_route(dict(candidate, account=ident), readings={}) is None
            for used, expected in ((0, True), (89, True), (90, False), (100, False), ("bad", True)):
                assert bool(accounts.next_account_route(candidate, readings={"third": {"weekly_used": used}})) is expected
            assert accounts.next_account_route(dict(candidate, model="unsupported"), readings={}) is None
        with patch.object(accounts, "load_accounts", return_value={"configured": False, "providers": {}}):
            assert accounts.next_account_route(candidate) is None

exercise_provider_account_execution()
# END Provider account execution

# BEGIN Provider account retry evidence
def exercise_account_retry_evidence():
    import panel_dispatch as dispatch
    import panel_results
    import provider_accounts as accounts
    from tempfile import TemporaryDirectory
    from types import SimpleNamespace
    with TemporaryDirectory(dir=temporary) as directory:
        home = Path(directory)
        entry = {"id": "credits", "home": str(home)}
        if hasattr(os, "getuid"):
            try:
                for mode, unsafe in ((0o700, False), (0o755, False), (0, False),
                                     (0o702, True), (0o720, True), (0o777, True)):
                    home.chmod(mode)
                    error = accounts.account_home_error(entry)
                    assert bool(error) is unsafe, (mode, error)
                    if unsafe:
                        assert "credits" in error and "chmod go-w" in error, error
                home.chmod(0o700)
                with patch.object(accounts.os, "getuid", return_value=home.stat().st_uid + 1):
                    error = accounts.account_home_error(entry)
                    assert error and "credits" in error and "ownership" in error, error
                home.chmod(0o777)
                platform = SimpleNamespace(**{key: value for key, value in vars(os).items() if key != "getuid"})
                with patch.object(accounts, "os", platform):
                    assert accounts.account_home_error(entry) is None
            finally:
                home.chmod(0o700)
    attempt = {"attempt_id": "failed", "task_id": "overflow", "reason_code": "provider_failed", "refs": {
        "panel_room_participant": "worker", "provider_error": "capacity", "provider_error_provider": "codex",
        "provider_error_exit": "1"}}
    indexed = {"attempt_id": "failed", "verify_exit": 0}
    with patch.object(panel, "events", return_value=json.dumps([attempt])) as writer:
        assert dispatch._record_deferred_fallback(project, "overflow", set(), "worker")
        first_write = writer.call_args
        assert "panel_fallback_decision=deferred" in first_write.args
        assert "--idempotency-key" in first_write.args and "failed" in first_write.args[-1]
        assert dispatch._record_deferred_fallback(project, "overflow", set(), "worker")
        assert writer.call_args == first_write
        for rows in ([], [attempt, attempt], [None], [{}], [dict(attempt, attempt_id=[])],
                     [dict(attempt, attempt_id="")], [dict(attempt, refs=None)]):
            writer.reset_mock()
            writer.return_value = json.dumps(rows)
            assert not dispatch._record_deferred_fallback(project, "overflow", set(), "worker"), rows
            assert writer.call_count == 1
        writer.return_value = json.dumps([attempt])
        assert not dispatch._record_deferred_fallback(project, "overflow", {"failed"}, "worker")
        assert not dispatch._record_deferred_fallback(project, "overflow", set(), "other")
    with patch.object(panel_results, "_records", return_value=([attempt], [indexed])) as records, \
            patch.object(panel_results, "verified_patch", return_value=b"") as content:
        def admitted(access="write", before=None, member="worker"):
            return dispatch._account_capacity_retry(project, "overflow", before or set(), member, access)
        assert admitted() and admitted("read")
        assert not admitted(before={"failed"}) and not admitted(member="other")
        for rows, calls in (([], [indexed]), ([attempt, attempt], [indexed]), ([attempt], []), ([attempt], [indexed, indexed])):
            records.return_value = rows, calls
            assert not admitted()
        records.return_value = [attempt], [indexed]
        for code in (None, "", "bad", "0", 0, True, -1, 256):
            with patch.dict(attempt["refs"], provider_error_exit=code):
                assert not admitted(), code
        for code in (1, "1", 255, "255"):
            with patch.dict(attempt["refs"], provider_error_exit=code):
                assert admitted(), code
        for key, values in (("provider_error", (None, "policy", "model_unavailable", "provider_down")),
                            ("provider_error_provider", (None, "claude"))):
            for value in values:
                with patch.dict(attempt["refs"], {key: value}):
                    assert not admitted()
        with patch.dict(attempt, reason_code="verification_failed"):
            assert not admitted()
        for key, values in (("verify_exit", (False, -1, 1, 7, 124, 125, "0", "bad")),
                            ("fallback_reason", ("policy-declined", "capacity-dirty-worktree", "capacity")),
                            ("fallback_used", (True,)), ("guard", ("blocked",))):
            for value in values:
                with patch.dict(indexed, {key: value}):
                    assert not admitted(), (key, value)
        for code in (None, 0):
            with patch.dict(indexed, verify_exit=code):
                assert admitted()
        for category in ("capacity", "model_unavailable", "provider_down"):
            with patch.dict(attempt["refs"], provider_error_provider="claude", provider_error=category):
                assert dispatch._account_capacity_retry(project, "overflow", set(), "worker", "write", "claude")
            with patch.dict(attempt["refs"], provider_error=category):
                assert dispatch._account_capacity_retry(project, "overflow", set(), "worker", "write", capacity_only=False)
        with patch.dict(indexed, fallback_reason="model-unavailable"), patch.dict(attempt["refs"], provider_error="model_unavailable"):
            assert dispatch._account_capacity_retry(project, "overflow", set(), "worker", "write", capacity_only=False)
        content.return_value = b"changed"
        assert not admitted()
    implicit = {"provider": "codex", "account": "default", "account_reason": "explicit target; default current home"}
    assert accounts.implicit_default_account(implicit)
    assert accounts.route_account_home(implicit) is None
    for reason in (None, "", 0, False, [], {}, "ordered", "default current home"):
        assert not accounts.implicit_default_account(dict(implicit, account_reason=reason))
    assert not accounts.implicit_default_account(dict(implicit, account="configured"))
    previous = {"attempt_id": "worker", "parent_attempt_id": "main", "provider": "codex", "refs": {
        "panel_room_participant": "call-legacy", "panel_account": "credits", "native_session": "native-session"}}
    reached = RuntimeError("fixture reached results")
    with patch.object(panel, "work_repository", side_effect=Path), \
            patch.object(panel, "owning_attempt", return_value=["main"]), \
            patch.object(panel, "events", return_value=json.dumps([{"attempt_id": "main", "terminal": False}, previous])), \
            patch.object(panel, "_rework_history", return_value=[previous]), \
            patch.object(panel, "results", side_effect=reached) as result_view:
        outcomes = [(error("fixture anchor unavailable"), False) for error in (OSError, ValueError, KeyError, TypeError)]
        outcomes += [(value, False) for value in (None, {}, [], {"route": None}, {"route": []},
            {"route": {"account": "credits"}})]
        outcomes += [({"route": {"account": "credits", "account_applied": value}}, False)
                     for value in (None, False, 0, 1, "true", [], {})]
        outcomes += [({"route": {"account": value, "account_applied": True}}, False)
                     for value in (None, "", 0, [], {})]
        outcomes.append(({"route": {"account": "credits", "account_applied": True}}, True))
        failures = []
        for outcome, applied in outcomes:
            options = {"side_effect": outcome} if isinstance(outcome, Exception) else {"return_value": outcome}
            with patch.object(dispatch.dispatch_store, "read", **options):
                route = {"provider": "codex", "account": "pro", "account_reason": "ordered"}
                try:
                    dispatch.continuation(project, "claude", None, "account-anchor", route, brief, {})
                    raise AssertionError("continuation skipped results")
                except RuntimeError as error:
                    assert error is reached, error
                except (OSError, ValueError, KeyError, TypeError) as error:
                    failures.append((type(error).__name__, "anchor read escaped"))
                    continue
                if bool(route.get("account")) is not applied:
                    failures.append((outcome, "incorrect account binding"))
        result_view.side_effect = None
        result_view.return_value = {"rows": []}
        with patch.object(dispatch.dispatch_store, "read", return_value={"route": {"account": "credits", "account_applied": True}}), \
                patch.object(panel_results, "_records", return_value=([], [])), \
                patch.object(accounts, "route_account_home", side_effect=ValueError("no longer configured: credits")):
            try:
                dispatch.continuation(project, "claude", None, "account-anchor", {"provider": "codex"}, brief, {})
                raise AssertionError("removed account resumed")
            except ValueError as error:
                if "non-Codex provider" not in str(error) or "routed to another provider" not in str(error):
                    failures.append((str(error), "incorrect recovery advice"))
        assert not failures, failures
    from panel_routing import allocate
    route = dict(allocate("claude", role="worker", tier="deep", access="read", installed={"codex"},
                          resolve_accounts=False), account="missing")
    with patch.dict(os.environ, {"OMS_ROOM_REPO": "", "OMS_ROOM_ID": "", "OMS_ROOM_PARTICIPANT": ""}), \
            patch.object(panel, "child_environment", return_value={}), \
            patch.object(dispatch.room, "resolve", return_value=None), \
            patch.object(panel, "plan_link", return_value=None), \
            patch.object(panel, "_print_dispatch_next_ready"), \
            patch.object(panel, "_prepare_dispatch_call", side_effect=AssertionError("anchor preceded home check")) as anchor, \
            patch.object(accounts, "next_account_route", return_value=None, create=True), \
            patch.object(accounts, "route_account_home", side_effect=ValueError("invalid home: missing"), create=True):
        try:
            dispatch.run_dispatch(project, "claude", "worker", "read", "Inspect.", None, None,
                                  "account-invalid-home", False, None, route, None, None, [])
            raise AssertionError("invalid home launched")
        except ValueError as error:
            assert "invalid home: missing" in str(error), error
        assert not anchor.called

exercise_account_retry_evidence()
# END Provider account retry evidence
