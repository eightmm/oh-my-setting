# Settled results are structured room events, and review admission remains with the main.
for owner in ("pi", "antigravity"):
    for model, expected in (("anthropic/claude-opus-5-5", "codex"),
                            ("openai/gpt-6.1-sol", "claude")):
        assert allocate(owner, role="reviewer", installed={"claude", "codex"},
                        owner_model=model)["provider"] == expected
    for model in (None, "", "unclassified-model"):
        try:
            allocate(owner, role="reviewer", installed={"claude", "codex"}, owner_model=model)
        except ValueError as error:
            assert "family" in str(error), error
        else:
            raise AssertionError("unknown model family established independence")
    assert allocate(owner, role="reviewer", installed={"claude", "codex"},
                    writer="codex")["provider"] == "claude"
import panel_routing as family_routing
with patch.object(family_routing, "model_family", side_effect=lambda provider, model:
                  {"pi": "anthropic", "claude": "anthropic", "codex": "openai"}[provider]) as family_lookup:
    assert allocate("pi", role="reviewer", installed={"claude", "codex"},
                    owner_model="anthropic/claude-opus-5-5")["provider"] == "codex"
    family_lookup.assert_called_once_with("pi", "anthropic/claude-opus-5-5")
# Keep automatic-review calls out of the shared room used by the existing layout assertions.
shared_before_auto, room_env_before_auto = shared, room_env
shared = room.create(project, "auto-review-room", "Automatic review fixtures")
room.join(project, shared, room_owner, "claude", "main", model="claude-opus-5-5")
room_env = dict(room_env, OMS_ROOM_ID=shared)
import panel_results as panel_result_store
import panel_review as panel_review_impl
Path(room_env["XDG_STATE_HOME"]).mkdir(mode=0o700, parents=True, exist_ok=True)
Path(room_env["XDG_STATE_HOME"]).chmod(0o700)
def fixture_call_id(label):
    return "call-" + hashlib.sha256(label.encode("utf-8")).hexdigest()[:16]

auto_task, auto_call, auto_attempt = "auto-review-task", "call-aaaaaaaaaaaaaaaa", "attempt-auto-review-fixture"
auto_patch = b"diff --git a/scripts/fixture.py b/scripts/fixture.py\nindex 1111111..2222222 100644\n--- a/scripts/fixture.py\n+++ b/scripts/fixture.py\n@@ -1 +1 @@\n-old\n+new\n"
auto_digest = hashlib.sha256(auto_patch).hexdigest()
assert panel._automatic_review_workload({"changed_files": ["docs/TERMINAL-PANEL.md", "scripts/fixture.py"]}) == "deep"
for boundary in ("hooks/pre-commit", "plugins/oh-my-setting/hooks.json", "install.sh", "scripts/patch-land.sh",
                 "scripts/patch-admit.sh", "scripts/resume-hook.sh", "scripts/lib/pi-oms-extension.mjs",
                 "scripts/lib/hook_state.py", "scripts/install-claude-hooks.sh"):
    assert panel._automatic_review_workload({"changed_files": ["docs/x.md", boundary]}) == "frontier", boundary
assert panel._automatic_review_workload({"changed_files": []}) == "frontier"
assert panel._automatic_review_workload({"changed_files": ["../escape"]}) == "frontier"
assert panel_review_impl._security_audit_paths(["scripts/permissions.sh"], None) == ["scripts/permissions.sh"]
assert panel_review_impl._security_audit_paths(["docs/permissions.md"], None) == ["docs/permissions.md"]
assert panel_review_impl._security_audit_paths(["docs/readme.md"], None) == []
assert panel_review_impl._security_audit_paths(["rules/x.md"], None) == ["rules/x.md"]
assert panel_review_impl._security_audit_paths(["docs/cleanup.md"], b"+++ b/docs/cleanup.md\n+rm -rf old\n") == []
assert panel_review_impl._security_audit_paths(["src/x.py"], b"diff --git a/src/x.py b/src/x.py\n--- a/src/x.py\n+++ b/src/x.py\n@@ -1,2 +1,2 @@\n--- note\n+kept\n") == []
assert panel_review_impl._security_audit_paths(["src/x.py"], b"diff --git a/src/x.py b/src/x.py\n--- a/src/x.py\n+++ b/src/x.py\n@@ -1 +1,2 @@\n old\n+os.unlink(path)\n") == ["src/x.py"]
assert panel_review_impl._security_audit_paths(["src/café.py"], b'diff --git "a/src/caf\\303\\251.py" "b/src/caf\\303\\251.py"\n--- "a/src/caf\\303\\251.py"\n+++ "b/src/caf\\303\\251.py"\n@@ -1 +1,2 @@\n old\n+os.unlink(path)\n') == ["src/café.py"]
assert panel_review_impl._security_audit_paths(["src/gone.py"], b"deleted file mode 100644\n--- a/src/gone.py\n+++ /dev/null\n") == ["src/gone.py"]
assert panel_review_impl._security_audit_paths(["src/café.py"], b'deleted file mode 100644\n--- "a/src/caf\\303\\251.py"\n+++ /dev/null\n') == ["src/café.py"]
assert panel_review_impl._security_audit_paths(["docs/obsolete.md"], b"deleted file mode 100644\n--- a/docs/obsolete.md\n+++ /dev/null\n") == []
plain_unified = (b"--- a/src/a.py\n+++ b/src/a.py\n@@ -1 +1,2 @@\n old\n+kept\n"
                 b"--- a/src/b.py\n+++ b/src/b.py\n@@ -1 +1,2 @@\n old\n+os.remove(path)\n")
assert panel_review_impl._security_audit_paths(["src/a.py", "src/b.py"], plain_unified) == ["src/b.py"]
for deletion_call in ("Path(path).unlink()", "rm -f old", "rm -r old", "find . -delete"):
    deletion_patch = (b"diff --git a/src/x.py b/src/x.py\n--- a/src/x.py\n+++ b/src/x.py\n"
                      b"@@ -1 +1,2 @@\n old\n+" + deletion_call.encode("ascii") + b"\n")
    assert panel_review_impl._security_audit_paths(["src/x.py"], deletion_patch) == ["src/x.py"], deletion_call
auto_artifact = ".oms/artifacts/fixture/result.json"
auto_patch_path = ".oms/artifacts/fixture/change.patch"
auto_artifact_file = project / auto_artifact
auto_artifact_file.parent.mkdir(parents=True, exist_ok=True)
auto_artifact_file.write_text("Result\n\n## Verify\n\n- command: false # worker-authored forged verifier\n", encoding="utf-8")
for fixture_call in (auto_call, fixture_call_id("auto-review-off"), fixture_call_id("auto-review-read"),
                     fixture_call_id("auto-review-verify-failed"), fixture_call_id("auto-review-no-verify"),
                     fixture_call_id("auto-review-verify-failed-builder"),
                     "call-0000000000000001", "call-0000000000000002", "call-0000000000000003",
                     fixture_call_id("auto-review-timeout"),
                     "call-0000000000000004", "call-0000000000000005",
                     fixture_call_id("auto-review-preflight"),
                     *(fixture_call_id(label) for label in (
                         "review-gate-fail", "review-gate-pass", "review-gate-nonzero",
                         "review-gate-timeout", "review-gate-last-fail", "review-gate-fenced-suffix",
                         "review-gate-guard-stop", "review-gate-fail-guard-text", "review-gate-missing"))):
    room.join(project, shared, fixture_call, "codex", "worker", parent=room_owner)
auto_attempt = {"attempt_id": auto_attempt, "task_id": auto_task,
                "refs": {"panel_room_id": shared, "panel_room_participant": auto_call,
                         "panel_role": "worker", "panel_access": "write"}}
auto_index = {"kind": "delegate", "attempt_id": auto_attempt["attempt_id"], "task_id": auto_task,
              "artifact": auto_artifact, "patch": auto_patch_path, "patch_sha256": auto_digest,
              "artifact_sha256": hashlib.sha256(auto_artifact_file.read_bytes()).hexdigest(),
              "verify_exit": 0,
              "base_sha": subprocess.check_output(["git", "-C", str(project), "rev-parse", "HEAD"], text=True).strip()}
run_dispatch = panel.run_dispatch
actual_dispatch_calls = []
def fixture_dispatch(*args, **kwargs):
    if args[8]:
        return run_dispatch(*args, **kwargs)
    actual_dispatch_calls.append((args, kwargs))
    return 0
from panel_routing import command as routed_command
catalog_rows = [dict(provider=provider, installed=provider == "codex", binary=provider, access=["read", "write"],
                     catalog_models=[], models=[], exact_model_override=True)
                for provider in ("codex", "claude")]
for main_owner, main_model, expected in (("pi", "anthropic/claude-opus-5-5", "codex"),
                                       ("pi", "openai/gpt-6.1-sol", "claude"),
                                       ("antigravity", "claude-opus-5-5", "codex")):
    with patch.object(panel, "provider_catalog", return_value=[dict(row, installed=True) for row in catalog_rows]), \
            patch.object(panel, "active_mains", return_value={room_owner: {"refs": {"panel_model": main_model}}}), \
            patch.dict(os.environ, room_env, clear=True):
        selected = panel.selected_route(main_owner, "reviewer", None, "read", None,
                                        repo=project, routing_readings={})
    assert selected["owner"] == main_owner and selected["provider"] == expected, selected
with patch.object(panel, "provider_catalog", return_value=catalog_rows):
    route = panel.selected_route("claude", "reviewer", None, "read", None, repo=project)
review_prompt = "Review task auto-review-task. Read repo-relative patch .oms/artifacts/fixture/change.patch.\nDo not apply it."
worker_execution_repo = project.parent / "worker-execution-repo"
worker_execution_repo.mkdir(exist_ok=True)
command = routed_command(panel.ENTRY, project, route, prompt=review_prompt,
                         verify="test -f scripts/fixture.py", task_id=auto_task)
assert command[command.index("--verify") + 1] == "test -f scripts/fixture.py"
recorded_call = panel._prepare_dispatch_call(project, shared, "call-0000000000000000", room_owner,
                                             "test -f scripts/fixture.py", project)
assert panel._dispatch_call_record(project, shared, recorded_call) == recorded_call
assert panel._dispatch_verifier(project, shared, recorded_call["participant"], room_owner) == \
    "test -f scripts/fixture.py"
with patch.object(panel_result_store, "_records", return_value=([auto_attempt], [auto_index])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.object(panel, "work_repository", return_value=worker_execution_repo), \
        patch.object(panel, "provider_catalog", return_value=catalog_rows), \
        patch.dict(os.environ, room_env, clear=True):
    panel._prepare_dispatch_call(project, shared, auto_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    first_event, _ = panel._call_result_event(project, shared, auto_call, auto_task, 0)
    second_event, _ = panel._call_result_event(project, shared, auto_call, auto_task, 0)
    assert first_event == second_event and first_event["needs_review"]
    assert first_event["changed_files"] == ["scripts/fixture.py"]
    assert first_event["patch_sha256"] == auto_digest and first_event["verify_status"] == "passed"
    assert (first_event["task_id"], first_event["call_id"], first_event["role"], first_event["access"],
            first_event["exit"], first_event["artifact"], first_event["patch"], first_event["verify_exit"]) == (
                auto_task, auto_call, "worker", "write", 0, auto_artifact, auto_patch_path, 0)
    panel.deliver_result(project, shared, auto_call, room_owner, auto_task, 0)
    panel.deliver_result(project, shared, auto_call, room_owner, auto_task, 0)
    assert len(actual_dispatch_calls) == 1, actual_dispatch_calls
    dispatch_args, dispatch_kwargs = actual_dispatch_calls[0]
    assert dispatch_args[6] == "test -f scripts/fixture.py"
    prompt = dispatch_args[4]
    assert auto_artifact in prompt and auto_patch_path in prompt and str(project) not in prompt
    assert "do not regenerate it with git diff" in prompt and "direct callers and callees, and their existing tests" in prompt
    assert dispatch_args[2:4] == ("reviewer", "read")
    assert dispatch_args[10]["workload"] == "deep", dispatch_args[10]
    assert dispatch_args[10]["provider"] != "claude", dispatch_args[10]
    assert set(dispatch_kwargs) == {"candidate", "routing_report", "routing_repo"}, dispatch_kwargs
    assert dispatch_kwargs["candidate"] == {"patch": auto_patch_path, "sha256": auto_digest,
                                             "base": auto_index["base_sha"]}, dispatch_kwargs
    assert isinstance(dispatch_kwargs["routing_report"], list)
    assert len(dispatch_kwargs["routing_report"]) == 1
    assert isinstance(dispatch_kwargs["routing_report"][0], dict)
    assert dispatch_kwargs["routing_report"][0].get("kind") == "oms-dashboard"
    assert Path(dispatch_kwargs["routing_repo"]).resolve() == project.resolve()
    assert dispatch_args[15] == room.review_ids(auto_call, auto_digest, 1)[2]
    assert dispatch_args[14] == worker_execution_repo and dispatch_args[14] != project, dispatch_args
    result_events = [row["room_event"] for row in room.records(project, shared)
                     if row.get("room_event", {}).get("kind") == "call-result"
                     and row["room_event"].get("call_id") == auto_call]
    assert len(result_events) == 1 and result_events[0]["attempt_id"] == auto_attempt["attempt_id"]
    result_handoff = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-" + auto_call)
    assert not result_handoff["pending_for"] and result_handoff.get("quiet") is True
    assert "call-result-" + hashlib.sha256((auto_call + "\0" + auto_attempt["attempt_id"]).encode()).hexdigest() in {
        row.get("room_event", {}).get("event_id") for row in room.records(project, shared)}
    # A reserved review with no reviewer launch is reusable after owner death.
    crash_review_call = "call-d0d0d0d0d0d0d0d0"
    crash_review_attempt = dict(auto_attempt, attempt_id="attempt-review-reservation-retry",
        refs=dict(auto_attempt["refs"], panel_room_participant=crash_review_call))
    crash_review_patch = auto_patch + b"# reservation recovery\n"
    crash_review_digest = hashlib.sha256(crash_review_patch).hexdigest()
    crash_review_index = dict(auto_index, attempt_id=crash_review_attempt["attempt_id"],
                              patch_sha256=crash_review_digest)
    room.join(project, shared, crash_review_call, "codex", "worker", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, crash_review_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    review_launch_attempts = []
    successful_review_launches = []
    def crash_after_review_reservation(*args, **kwargs):
        if kwargs.get("dry_run"):
            return 0
        review_launch_attempts.append(args)
        if len(review_launch_attempts) == 1:
            raise SystemExit(7)
        successful_review_launches.append(args)
        return 0
    with patch.object(panel_result_store, "_records", return_value=([crash_review_attempt], [crash_review_index])), \
            patch.object(panel_result_store, "verified_patch", return_value=crash_review_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "dispatch", side_effect=crash_after_review_reservation), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        try:
            panel.deliver_result(project, shared, crash_review_call, room_owner, auto_task, 0)
        except SystemExit as error:
            assert error.code == 7
        else:
            raise AssertionError("review reservation crash fixture did not stop before launch")
        reservation = next(row["room_event"] for row in room.records(project, shared)
                           if row.get("room_event", {}).get("kind") == "review-dispatch"
                           and row["room_event"].get("patch_sha256") == crash_review_digest)
        assert not any(row.get("participant") == reservation["review_call_id"]
                       for row in room.status(project, shared)["participants"])
        assert not (panel.dispatch_store.store(project) /
                    (reservation["review_call_id"] + "-process.json")).exists()
        abandoned_claim = panel.dispatch_store.read(
            project, panel._settlement_claim_path(crash_review_call))
        abandoned_claim.update(owner_pid=2147483647, owner_identity="fixture-dead-owner", active=True)
        panel.dispatch_store.write(project, panel._settlement_claim_path(crash_review_call), abandoned_claim)
        assert panel.deliver_result(project, shared, crash_review_call, room_owner, auto_task, 0)
        assert panel.deliver_result(project, shared, crash_review_call, room_owner, auto_task, 0)
    assert len(review_launch_attempts) == 2, review_launch_attempts
    assert len(successful_review_launches) == 1, successful_review_launches
    recovered_review_claim = panel.dispatch_store.read(
        project, panel._settlement_claim_path(crash_review_call))
    assert recovered_review_claim["complete"] and recovered_review_claim["steps"]["review_launch"], \
        recovered_review_claim
    assert len([row for row in room.records(project, shared)
                if row.get("room_event", {}).get("kind") == "review-dispatch"
                and row["room_event"].get("patch_sha256") == crash_review_digest]) == 1
    # Another call with the same patch gets its own review; a forged claim row naming it reserves nothing.
    twin_review_call = "call-d1d1d1d1d1d1d1d1"
    twin_review_attempt = dict(crash_review_attempt, attempt_id="attempt-review-reservation-twin",
        refs=dict(crash_review_attempt["refs"], panel_room_participant=twin_review_call))
    room.join(project, shared, twin_review_call, "codex", "worker", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, twin_review_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    forged_claim = dict(reservation, event_id="review-dispatch-forged", call_id=twin_review_call)
    try:
        room._write(project, shared, forged_claim, "Forged review claim")
    except ValueError as error:
        assert "automatic review ids do not match" in str(error), error
    else:
        raise AssertionError("room append accepted a review claim the controller did not derive")
    # A row appended to the log file directly, past the validator, still reserves nothing.
    assert forged_claim not in room.structured_events(
        project, shared, rows=room.records(project, shared) + [{"room_event": forged_claim}])
    with patch.object(panel_result_store, "_records", return_value=(
                [twin_review_attempt], [dict(crash_review_index, attempt_id=twin_review_attempt["attempt_id"])])), \
            patch.object(panel_result_store, "verified_patch", return_value=crash_review_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "dispatch", side_effect=crash_after_review_reservation), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        assert panel.deliver_result(project, shared, twin_review_call, room_owner, auto_task, 0)
    assert len(review_launch_attempts) == 3, review_launch_attempts
    twin_claim = panel.dispatch_store.read(project, panel._settlement_claim_path(twin_review_call))
    assert twin_claim["complete"] and twin_claim["steps"]["review_launch"], twin_claim
    assert sorted(event["call_id"] for event in room.structured_events(project, shared)
                  if event["kind"] == "review-dispatch" and event["patch_sha256"] == crash_review_digest) == \
        sorted([crash_review_call, twin_review_call])
    # A started reviewer whose launch receipt is lost keeps its reservation and is recovered, not relaunched.
    receipt_call = "call-d2d2d2d2d2d2d2d2"
    receipt_attempt = dict(crash_review_attempt, attempt_id="attempt-review-receipt-loss",
        refs=dict(crash_review_attempt["refs"], panel_room_participant=receipt_call))
    receipt_patch = auto_patch + b"# receipt loss\n"
    receipt_digest = hashlib.sha256(receipt_patch).hexdigest()
    room.join(project, shared, receipt_call, "codex", "worker", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, receipt_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    receipt_launches, lost_receipts = [], []
    def launch_with_process_record(*args, **kwargs):
        if not kwargs.get("dry_run"):
            receipt_launches.append(kwargs["member_id"])
            panel._prepare_dispatch_call(project, shared, kwargs["member_id"], room_owner)
            panel.dispatch_store.write(project, kwargs["member_id"] + "-process.json", {"pid": os.getpid()})
        return 0
    real_settlement_step = panel._settlement_step
    def lose_launch_receipt(repo, member, claim, name, done=True):
        if name == "review_launch" and not lost_receipts:
            lost_receipts.append(member)
            raise OSError("fixture launch receipt loss")
        return real_settlement_step(repo, member, claim, name, done)
    with patch.object(panel_result_store, "_records", return_value=(
                [receipt_attempt], [dict(crash_review_index, attempt_id=receipt_attempt["attempt_id"],
                                         patch_sha256=receipt_digest)])), \
            patch.object(panel_result_store, "verified_patch", return_value=receipt_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "dispatch", side_effect=launch_with_process_record), \
            patch.object(panel, "_settlement_step", side_effect=lose_launch_receipt), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        assert not panel.deliver_result(project, shared, receipt_call, room_owner, auto_task, 0)
        assert not panel.dispatch_store.read(project, panel._settlement_claim_path(receipt_call))["complete"]
        # Unreadable launch evidence is not absence: the reservation stays and nothing relaunches.
        real_store_read = panel.dispatch_store.read
        def unreadable_launch_record(repo, name, *args, **kwargs):
            if name == receipt_launches[0] + "-process.json":
                raise OSError("fixture transient read failure")
            return real_store_read(repo, name, *args, **kwargs)
        with patch.object(panel.dispatch_store, "read", side_effect=unreadable_launch_record):
            assert not panel.deliver_result(project, shared, receipt_call, room_owner, auto_task, 0)
        assert panel.deliver_result(project, shared, receipt_call, room_owner, auto_task, 0)
    assert lost_receipts == [receipt_call] and len(receipt_launches) == 1, (lost_receipts, receipt_launches)
    receipt_events = [row["room_event"] for row in room.records(project, shared)
                      if row.get("room_event", {}).get("patch_sha256") == receipt_digest
                      and row["room_event"].get("kind") in {"review-dispatch", "review-dispatch-failed"}]
    assert [event["kind"] for event in receipt_events] == ["review-dispatch"], receipt_events
    receipt_claim = panel.dispatch_store.read(project, panel._settlement_claim_path(receipt_call))
    assert receipt_claim["complete"] and receipt_claim["steps"]["review_launch"], receipt_claim
    # Without a verifier the claim completes only after the manual-review notice is durable.
    notice_call = "call-d3d3d3d3d3d3d3d3"
    notice_attempt = dict(crash_review_attempt, attempt_id="attempt-review-notice-loss",
        refs=dict(crash_review_attempt["refs"], panel_room_participant=notice_call))
    notice_patch = auto_patch + b"# manual notice\n"
    room.join(project, shared, notice_call, "codex", "worker", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, notice_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    notice_launches, lost_notices = [], []
    real_room_send = room.send
    def lose_first_judgment(*args, **kwargs):
        if str(kwargs.get("message_id", "")).startswith("judgment-") and not lost_notices:
            lost_notices.append(kwargs["message_id"])
            raise OSError("fixture notice loss")
        return real_room_send(*args, **kwargs)
    with patch.object(panel_result_store, "_records", return_value=(
                [notice_attempt], [dict(crash_review_index, attempt_id=notice_attempt["attempt_id"],
                                        patch_sha256=hashlib.sha256(notice_patch).hexdigest())])), \
            patch.object(panel_result_store, "verified_patch", return_value=notice_patch), \
            patch.object(panel, "_review_verifier", return_value=None), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "dispatch", side_effect=lambda *a, **k: notice_launches.append(k)), \
            patch.object(room, "send", side_effect=lose_first_judgment), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        assert not panel.deliver_result(project, shared, notice_call, room_owner, auto_task, 0)
        lost_notice_claim = panel.dispatch_store.read(project, panel._settlement_claim_path(notice_call))
        assert not lost_notice_claim["complete"] and lost_notice_claim["steps"]["auto_review"] is False, \
            lost_notice_claim
        assert panel.deliver_result(project, shared, notice_call, room_owner, auto_task, 0)
    assert lost_notices == ["judgment-" + notice_call] and not notice_launches, (lost_notices, notice_launches)
    assert panel.dispatch_store.read(project, panel._settlement_claim_path(notice_call))["complete"]
    assert any(row.get("room_event", {}).get("id") == "judgment-" + notice_call
               for row in room.records(project, shared))
    # Past the spawn point (launch record written) a reserved reviewer is never relaunched; the main reviews.
    def review_kinds(digest):
        return [row["room_event"]["kind"] for row in room.records(project, shared)
                if row.get("room_event", {}).get("patch_sha256") == digest
                and row["room_event"].get("kind") in {"review-dispatch", "review-dispatch-failed"}]
    for spawn_call, spawn_failure in (("call-d4d4d4d4d4d4d4d4", SystemExit(9)),
                                      ("call-d5d5d5d5d5d5d5d5", ValueError("fixture process record refused"))):
        spawn_attempt = dict(crash_review_attempt, attempt_id="attempt-review-spawn-" + spawn_call[-4:],
            refs=dict(crash_review_attempt["refs"], panel_room_participant=spawn_call))
        spawn_patch = auto_patch + ("# spawn window %s\n" % spawn_call).encode()
        spawn_digest = hashlib.sha256(spawn_patch).hexdigest()
        room.join(project, shared, spawn_call, "codex", "worker", parent=room_owner)
        panel._prepare_dispatch_call(project, shared, spawn_call, room_owner,
                                     "test -f scripts/fixture.py", worker_execution_repo)
        spawn_launches = []
        def stop_after_launch_record(*args, **kwargs):
            if kwargs.get("dry_run"):
                return 0
            spawn_launches.append(kwargs["member_id"])
            panel._prepare_dispatch_call(project, shared, kwargs["member_id"], room_owner)
            raise spawn_failure
        with patch.object(panel_result_store, "_records", return_value=(
                    [spawn_attempt], [dict(crash_review_index, attempt_id=spawn_attempt["attempt_id"],
                                           patch_sha256=spawn_digest)])), \
                patch.object(panel_result_store, "verified_patch", return_value=spawn_patch), \
                patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
                patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
                patch.object(panel, "dispatch", side_effect=stop_after_launch_record), \
                patch.object(panel, "work_repository", return_value=worker_execution_repo), \
                patch.dict(os.environ, room_env, clear=True):
            if isinstance(spawn_failure, SystemExit):
                try:
                    panel.deliver_result(project, shared, spawn_call, room_owner, auto_task, 0)
                except SystemExit as error:
                    assert error.code == 9
                else:
                    raise AssertionError("spawn-window fixture did not stop after the launch record")
            assert panel.deliver_result(project, shared, spawn_call, room_owner, auto_task, 0)
        assert len(spawn_launches) == 1 and review_kinds(spawn_digest) == ["review-dispatch"], \
            (spawn_launches, review_kinds(spawn_digest))
        assert panel.dispatch_store.read(project, panel._settlement_claim_path(spawn_call))["complete"]
        assert any(row.get("room_event", {}).get("id") == "judgment-" + spawn_call
                   for row in room.records(project, shared))
    # A failed release write leaves the durable flag active; the same process still retries its delivery.
    release_call = "call-d6d6d6d6d6d6d6d6"
    release_attempt = dict(crash_review_attempt, attempt_id="attempt-review-release-loss",
        refs=dict(crash_review_attempt["refs"], panel_room_participant=release_call))
    release_patch = auto_patch + b"# release loss\n"
    room.join(project, shared, release_call, "codex", "worker", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, release_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    lost_notices[:] = []
    with patch.object(panel_result_store, "_records", return_value=(
                [release_attempt], [dict(crash_review_index, attempt_id=release_attempt["attempt_id"],
                                         patch_sha256=hashlib.sha256(release_patch).hexdigest())])), \
            patch.object(panel_result_store, "verified_patch", return_value=release_patch), \
            patch.object(panel, "_review_verifier", return_value=None), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        with patch.object(room, "send", side_effect=lose_first_judgment), \
                patch.object(panel, "_settlement_release", side_effect=OSError("fixture release loss")):
            assert not panel.deliver_result(project, shared, release_call, room_owner, auto_task, 0)
        stranded = panel.dispatch_store.read(project, panel._settlement_claim_path(release_call))
        assert stranded["active"] and not stranded["complete"], stranded
        assert release_call not in panel.SETTLEMENTS_HELD
        assert panel.deliver_result(project, shared, release_call, room_owner, auto_task, 0)
    assert lost_notices == ["judgment-" + release_call], lost_notices
    assert panel.dispatch_store.read(project, panel._settlement_claim_path(release_call))["complete"]
    preflight_call = "call-0000000000000001"
    preflight_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-preflight",
        refs=dict(auto_attempt["refs"], panel_room_participant=preflight_call))
    preflight_patch = auto_patch + b"# reviewer preflight failure\n"
    preflight_digest = hashlib.sha256(preflight_patch).hexdigest()
    preflight_index = dict(auto_index, attempt_id=preflight_attempt["attempt_id"], patch_sha256=preflight_digest)
    panel._prepare_dispatch_call(project, shared, preflight_call, room_owner,
                                 "test -f scripts/fixture.py", worker_execution_repo)
    with patch.object(panel_result_store, "_records", return_value=([preflight_attempt], [preflight_index])), \
            patch.object(panel_result_store, "verified_patch", return_value=preflight_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "dispatch", side_effect=ValueError("review preflight refused")), \
            patch.object(panel, "work_repository", return_value=worker_execution_repo), \
            patch.dict(os.environ, room_env, clear=True):
        panel.deliver_result(project, shared, preflight_call, room_owner, auto_task, 0)
    preflight_review = next(row["room_event"] for row in room.records(project, shared)
                            if row.get("room_event", {}).get("kind") == "review-dispatch"
                            and row["room_event"].get("patch_sha256") == preflight_digest)
    preflight_member = preflight_review["review_call_id"]
    room.join(project, shared, preflight_member, "codex", "reviewer", parent=room_owner)
    panel._prepare_dispatch_call(project, shared, preflight_member, room_owner)
    preflight_result, _ = panel._call_result_event(project, shared, preflight_member, "wrong-task", 1)
    assert preflight_result["role"] == "reviewer" and preflight_result["task_id"] == auto_task, preflight_result
    panel.deliver_result(project, shared, preflight_member, room_owner, "wrong-task", 1)
    reviewer_mail = next(m for m in room.status(project, shared)["messages"]
                         if m["id"] == "result-" + preflight_member)
    assert "review failed or is inconclusive" in reviewer_mail["text"] and room_owner in reviewer_mail["targets"]
    reviewer_result = next(row["room_event"] for row in room.records(project, shared)
                           if row.get("room_event", {}).get("kind") == "call-result"
                           and row["room_event"].get("call_id") == preflight_member)
    assert reviewer_result["role"] == "reviewer" and reviewer_result["task_id"] == auto_task
    preflight_mail = next(m for m in room.status(project, shared)["messages"]
                          if m["id"] == "judgment-" + preflight_call)
    assert "could not launch automatic review" in preflight_mail["text"] and room_owner in preflight_mail["targets"]
    assert any(row.get("room_event", {}).get("kind") == "review-dispatch-failed"
               and row["room_event"].get("review_call_id") == preflight_member for row in room.records(project, shared))
    auto_off_call = fixture_call_id("auto-review-off")
    panel._prepare_dispatch_call(project, shared, auto_off_call, room_owner)
    with patch.object(panel_result_store, "_records", return_value=([dict(auto_attempt,
            attempt_id="attempt-auto-review-off", refs=dict(auto_attempt["refs"], panel_room_participant=auto_off_call,
                                                              panel_role="worker", panel_purpose="implement"))],
            [dict(auto_index, attempt_id="attempt-auto-review-off")])), \
            patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "é🙂" * 3000}]}]}), \
            patch.object(panel, "policy", return_value={"auto_review": False}), \
            patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
            patch.dict(os.environ, room_env, clear=True):
        panel.deliver_result(project, shared, auto_off_call, room_owner, auto_task, 0)
        assert len(actual_dispatch_calls) == 1
        legacy = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-" + auto_off_call)
        assert ("[Summary truncated.] Full answer: %s. Results overview: "
                "oms panel --results --task-id %s --participant %s" %
                (auto_artifact, auto_task, auto_off_call)) in legacy["text"]
        assert (project / auto_artifact).is_file()
        assert len(legacy["text"].encode("utf-8")) <= 4000
        assert legacy["text"].encode("utf-8").decode("utf-8") == legacy["text"]
        assert not legacy.get("quiet", False)
read_call = fixture_call_id("auto-review-read")
panel._prepare_dispatch_call(project, shared, read_call, room_owner)
with patch.object(panel_result_store, "_records", return_value=([dict(auto_attempt,
        attempt_id="attempt-auto-review-read", refs=dict(auto_attempt["refs"], panel_room_participant=read_call,
                                                         panel_access="read"))],
        [dict(auto_index, attempt_id="attempt-auto-review-read")])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, read_call, room_owner, auto_task, 0)
    assert len(actual_dispatch_calls) == 1
read_timeout_call = fixture_call_id("auto-review-read-timeout")
panel._prepare_dispatch_call(project, shared, read_timeout_call, room_owner)
room.join(project, shared, read_timeout_call, "codex", "worker", parent=room_owner)
read_timeout_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-read-timeout",
    refs=dict(auto_attempt["refs"], panel_room_participant=read_timeout_call, panel_access="read"))
with patch.object(panel_result_store, "_records", return_value=([read_timeout_attempt],
        [dict(auto_index, attempt_id=read_timeout_attempt["attempt_id"])])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker timed out."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, read_timeout_call, room_owner, auto_task, 124)
read_timeout_mail = next(m for m in room.status(project, shared)["messages"]
                         if m["id"] == "judgment-" + read_timeout_call)
assert "worker exited with status 124" in read_timeout_mail["text"]
assert "oms panel --salvage " not in read_timeout_mail["text"]
verify_call = fixture_call_id("auto-review-verify-failed")
panel._prepare_dispatch_call(project, shared, verify_call, room_owner)
verify_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-verify-failed",
    refs=dict(auto_attempt["refs"], panel_room_participant=verify_call,
              panel_role="worker", panel_purpose="implement"))
with patch.object(panel_result_store, "_records", return_value=([verify_attempt],
        [dict(auto_index, attempt_id=verify_attempt["attempt_id"], verify_exit=2)])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, verify_call, room_owner, auto_task, 1)
    assert len(actual_dispatch_calls) == 1
    verify_mail = next(m for m in room.status(project, shared)["messages"] if m["id"] == "judgment-" + verify_call)
    assert "verification failed" in verify_mail["text"] and room_owner in verify_mail["targets"]
    assert ("Continue with fixes: oms panel --dispatch worker --continue %s --access write "
            "--brief-file PATH --purpose implement --verify 'test -f scripts/fixture.py'" % auto_task) in verify_mail["text"]
    assert "Full answer: %s" % auto_artifact in verify_mail["text"]
    assert (project / auto_artifact).is_file()
    assert "include verifier diagnostics from the answer artifact in the continuation brief" in verify_mail["text"]
    assert "the failed verifier's tail carries over" not in verify_mail["text"]
    import io
    import shlex
    command_text = verify_mail["text"].split("Continue with fixes: ", 1)[1].split(" (Full answer:", 1)[0]
    command_args = shlex.split(command_text)
    try:
        with patch("sys.stdout", io.StringIO()):
            panel.main(command_args + ["--help"])
    except SystemExit as error:
        assert error.code == 0
    else:
        raise AssertionError("continuation command was not parsed by the panel CLI")
assert "oms panel --salvage " not in verify_mail["text"]
builder_verify_call = fixture_call_id("auto-review-verify-failed-builder")
panel._prepare_dispatch_call(project, shared, builder_verify_call, room_owner)
builder_verify_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-verify-failed-builder",
    refs=dict(auto_attempt["refs"], panel_room_participant=builder_verify_call,
              panel_role="worker", panel_purpose="builder"))
with patch.object(panel_result_store, "_records", return_value=([builder_verify_attempt],
        [dict(auto_index, attempt_id=builder_verify_attempt["attempt_id"], verify_exit=2)])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, builder_verify_call, room_owner, auto_task, 1)
builder_verify_mail = next(m for m in room.status(project, shared)["messages"]
                           if m["id"] == "judgment-" + builder_verify_call)
assert ("Continue with fixes: oms panel --dispatch builder --continue %s --access write "
        "--brief-file PATH --verify 'test -f scripts/fixture.py'" % auto_task) in builder_verify_mail["text"]
assert "--purpose" not in builder_verify_mail["text"]
command_text = builder_verify_mail["text"].split("Continue with fixes: ", 1)[1].split(" (Full answer:", 1)[0]
command_args = shlex.split(command_text)
try:
    with patch("sys.stdout", io.StringIO()):
        panel.main(command_args + ["--help"])
except SystemExit as error:
    assert error.code == 0
else:
    raise AssertionError("builder continuation command was not parsed by the panel CLI")
verify_long_call = fixture_call_id("auto-review-verify-failed-long-command")
panel._prepare_dispatch_call(project, shared, verify_long_call, room_owner)
room.join(project, shared, verify_long_call, "codex", "worker", parent=room_owner)
verify_long_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-verify-failed-long-command",
    refs=dict(auto_attempt["refs"], panel_room_participant=verify_long_call,
              panel_role="worker", panel_purpose="implement"))
long_verifier = "x" * 5000
with patch.object(panel_result_store, "_records", return_value=([verify_long_attempt],
        [dict(auto_index, attempt_id=verify_long_attempt["attempt_id"], verify_exit=2)])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value=long_verifier), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, verify_long_call, room_owner, auto_task, 1)
verify_long_mail = next(m for m in room.status(project, shared)["messages"]
                        if m["id"] == "judgment-" + verify_long_call)
assert ("Continue with fixes: oms panel --dispatch worker --continue %s --access write "
        "--brief-file PATH --purpose implement" % auto_task) in verify_long_mail["text"]
assert "--verify VERIFY" in verify_long_mail["text"]
assert "Full answer: %s" % auto_artifact in verify_long_mail["text"]
assert "results overview: oms panel --results --task-id %s --participant %s" % (auto_task, verify_long_call) in verify_long_mail["text"]
assert (project / auto_artifact).is_file()
assert len(verify_long_mail["text"].encode("utf-8")) <= 4000
verify_read_call = fixture_call_id("auto-review-verify-failed-read")
panel._prepare_dispatch_call(project, shared, verify_read_call, room_owner)
room.join(project, shared, verify_read_call, "codex", "worker", parent=room_owner)
verify_read_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-verify-failed-read",
    refs=dict(auto_attempt["refs"], panel_room_participant=verify_read_call, panel_access="read"))
with patch.object(panel_result_store, "_records", return_value=([verify_read_attempt],
        [dict(auto_index, attempt_id=verify_read_attempt["attempt_id"], verify_exit=2)])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, verify_read_call, room_owner, auto_task, 1)
verify_read_mail = next(m for m in room.status(project, shared)["messages"]
                        if m["id"] == "judgment-" + verify_read_call)
assert "verification failed" in verify_read_mail["text"]
assert "Continue with fixes:" not in verify_read_mail["text"]
verify_timeout_call = fixture_call_id("auto-review-verifier-timeout")
panel._prepare_dispatch_call(project, shared, verify_timeout_call, room_owner)
room.join(project, shared, verify_timeout_call, "codex", "worker", parent=room_owner)
verify_timeout_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-verifier-timeout",
    refs=dict(auto_attempt["refs"], panel_room_participant=verify_timeout_call, panel_access="write"))
with patch.object(panel_result_store, "_records", return_value=([verify_timeout_attempt],
        [dict(auto_index, attempt_id=verify_timeout_attempt["attempt_id"], verify_exit=124)])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Verifier timed out."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, verify_timeout_call, room_owner, auto_task, 1)
verify_timeout_mail = next(m for m in room.status(project, shared)["messages"]
                           if m["id"] == "judgment-" + verify_timeout_call)
assert "verification failed" in verify_timeout_mail["text"]
assert "Recover its edits: oms panel --salvage %s." % auto_task in verify_timeout_mail["text"]
no_verify_call = fixture_call_id("auto-review-no-verify")
panel._prepare_dispatch_call(project, shared, no_verify_call, room_owner)
no_verify_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-no-verify",
    refs=dict(auto_attempt["refs"], panel_room_participant=no_verify_call))
no_verify_index = dict(auto_index, attempt_id=no_verify_attempt["attempt_id"], artifact=".oms/artifacts/fixture/no-verify.json")
(project / no_verify_index["artifact"]).write_text("Result without verifier\n", encoding="utf-8")
no_verify_index["artifact_sha256"] = hashlib.sha256((project / no_verify_index["artifact"]).read_bytes()).hexdigest()
with patch.object(panel_result_store, "_records", return_value=([no_verify_attempt], [no_verify_index])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value=None), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, no_verify_call, room_owner, auto_task, 0)
    assert len(actual_dispatch_calls) == 1
    no_verify_mail = next(m for m in room.status(project, shared)["messages"]
                          if m["id"] == "judgment-" + no_verify_call)
    assert "no recorded verifier" in no_verify_mail["text"] and room_owner in no_verify_mail["targets"]
for malformed_tasks in ([], None):
    with patch.object(panel, "_dispatch_verifier", return_value=None), \
            patch.object(panel_result_store, "_read", return_value=json.dumps({"tasks": malformed_tasks})):
        assert panel._review_verifier(project, shared, auto_call, room_owner, auto_task) is None
for missing_repo_call, execution_repo in (
        ("call-0000000000000002", None),
        ("call-0000000000000003", project.parent / "missing-worker-execution-repo")):
    missing_repo_attempt = dict(auto_attempt, attempt_id="attempt-" + missing_repo_call,
        refs=dict(auto_attempt["refs"], panel_room_participant=missing_repo_call))
    missing_repo_index = dict(auto_index, attempt_id=missing_repo_attempt["attempt_id"])
    panel._prepare_dispatch_call(project, shared, missing_repo_call, room_owner,
                                 "test -f scripts/fixture.py", execution_repo)
    with patch.object(panel_result_store, "_records", return_value=([missing_repo_attempt], [missing_repo_index])), \
            patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
            patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
            patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
            patch.dict(os.environ, room_env, clear=True):
        before_calls = len(actual_dispatch_calls)
        panel.deliver_result(project, shared, missing_repo_call, room_owner, auto_task, 0)
        assert len(actual_dispatch_calls) == before_calls
    missing_repo_mail = next(m for m in room.status(project, shared)["messages"]
                             if m["id"] == "judgment-" + missing_repo_call)
    assert "execution repository is unknown or no longer available" in missing_repo_mail["text"]
    assert room_owner in missing_repo_mail["targets"]
    missing_repo_handoff = next(m for m in room.status(project, shared)["messages"]
                                if m["id"] == "result-" + missing_repo_call)
    assert missing_repo_handoff.get("quiet") is not True
    assert not any(row.get("room_event", {}).get("kind") == "review-dispatch"
                   and row["room_event"].get("call_id") == missing_repo_call
                   for row in room.records(project, shared))
timeout_call = fixture_call_id("auto-review-timeout")
panel._prepare_dispatch_call(project, shared, timeout_call, room_owner)
timeout_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-timeout",
    refs=dict(auto_attempt["refs"], panel_room_participant=timeout_call, panel_access="write"))
timeout_index = dict(auto_index, attempt_id=timeout_attempt["attempt_id"], verify_exit=None)
with patch.object(panel_result_store, "_records", return_value=([timeout_attempt], [timeout_index])), \
        patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker timed out."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.dict(os.environ, room_env, clear=True):
    before_calls = len(actual_dispatch_calls)
    panel.deliver_result(project, shared, timeout_call, room_owner, auto_task, 124)
    assert len(actual_dispatch_calls) == before_calls
timeout_messages = room.status(project, shared)["messages"]
timeout_mail = next(m for m in timeout_messages if m["id"] == "judgment-" + timeout_call)
timeout_handoff = next(m for m in timeout_messages if m["id"] == "result-" + timeout_call)
assert "worker exited with status 124" in timeout_mail["text"] and room_owner in timeout_mail["targets"]
assert "Recover its edits: oms panel --salvage %s." % auto_task in timeout_mail["text"]
assert not timeout_handoff.get("quiet") and timeout_mail["seq"] < timeout_handoff["seq"]
plan_verify_call = "call-0000000000000004"
plan_verify_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-plan-verify",
    refs=dict(auto_attempt["refs"], panel_room_participant=plan_verify_call))
plan_artifact = ".oms/artifacts/fixture/plan-verifier.json"
(project / plan_artifact).write_text("Result without call verifier\n", encoding="utf-8")
plan_patch = auto_patch + b"# plan verifier digest\n"
plan_verify_index = dict(auto_index, attempt_id=plan_verify_attempt["attempt_id"], artifact=plan_artifact,
                         patch_sha256=hashlib.sha256(plan_patch).hexdigest())
plan_verify_index["artifact_sha256"] = hashlib.sha256((project / plan_artifact).read_bytes()).hexdigest()
plan_path = project / ".oms/plan/tasks.json"
plan_path.write_text(json.dumps({"tasks": {auto_task: {"verify": "test -f scripts/fixture.py"}}}), encoding="utf-8")
panel._prepare_dispatch_call(project, shared, plan_verify_call, room_owner,
                             "test -f scripts/fixture.py", worker_execution_repo)
with patch.object(panel_result_store, "_records", return_value=([plan_verify_attempt], [plan_verify_index])), \
        patch.object(panel_result_store, "verified_patch", return_value=plan_patch), \
        patch.object(panel, "_dispatch_verifier", return_value=None), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.object(panel, "work_repository", return_value=worker_execution_repo), \
        patch.object(panel, "provider_catalog", return_value=catalog_rows), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, plan_verify_call, room_owner, auto_task, 0)
assert len(actual_dispatch_calls) == 2 and actual_dispatch_calls[-1][0][6] == "test -f scripts/fixture.py", actual_dispatch_calls
plan_path.unlink()
retry_call = "call-0000000000000005"
retry_attempt = dict(auto_attempt, attempt_id="attempt-auto-review-retry",
    refs=dict(auto_attempt["refs"], panel_room_participant=retry_call))
retry_patch = auto_patch + b"# retry digest\n"
retry_digest = hashlib.sha256(retry_patch).hexdigest()
retry_index = dict(auto_index, attempt_id=retry_attempt["attempt_id"], patch_sha256=retry_digest)
retry_outputs = []
panel._prepare_dispatch_call(project, shared, retry_call, room_owner,
                             "test -f scripts/fixture.py", worker_execution_repo)
def fail_once(*args, **kwargs):
    if args[8]:
        return run_dispatch(*args, **kwargs)
    retry_outputs.append((args, kwargs))
    if len(retry_outputs) == 1:
        raise OSError("fixture launch failure")
    return 0
with patch.object(panel_result_store, "_records", return_value=([retry_attempt], [retry_index])), \
        patch.object(panel_result_store, "verified_patch", return_value=retry_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fail_once), \
        patch.object(panel, "work_repository", return_value=worker_execution_repo), \
        patch.object(panel, "provider_catalog", return_value=catalog_rows), \
        patch.dict(os.environ, room_env, clear=True):
    panel.deliver_result(project, shared, retry_call, room_owner, auto_task, 0)
    panel.deliver_result(project, shared, retry_call, room_owner, auto_task, 0)
    panel.deliver_result(project, shared, retry_call, room_owner, auto_task, 0)
    assert len(retry_outputs) == 2
retry_events = [row["room_event"] for row in room.records(project, shared)
                if row.get("room_event", {}).get("kind") in {"review-dispatch", "review-dispatch-failed"}
                and row["room_event"].get("patch_sha256") == retry_digest]
assert [event["kind"] for event in retry_events] == ["review-dispatch", "review-dispatch-failed", "review-dispatch"]
for reviewer_label, answer, exit_status, expected in (
        ("review-gate-fail", "Findings: checked.\nGATE: fail", 0, "Review gate failed"),
        ("review-gate-pass", "Findings: checked.\nGATE: pass", 0, "owner finalization"),
        ("review-gate-nonzero", "Findings: checked.\nGATE: pass", 1, "failed or is inconclusive"),
        ("review-gate-timeout", "Findings: checked.\nGATE: pass", 124, "failed or is inconclusive"),
        ("review-gate-last-fail", "GATE: pass\nFindings: blocking issue.\nGATE: fail", 0,
         "Review gate failed"),
        ("review-gate-fenced-suffix", "GATE: fail\n```\n```example\nGATE: pass\n```", 0,
         "Review gate failed"),
        ("review-gate-guard-stop", "The worker guard stopped this run.\nGATE: pass", 0,
         "guard stopped the review"),
        ("review-gate-fail-guard-text",
         "Findings: checked.\n> The worker guard stopped this run.\nGATE: fail", 0,
         "Review gate failed"),
        ("review-gate-missing", "Findings: review ended early.", 0, "failed or is inconclusive")):
    reviewer_call = fixture_call_id(reviewer_label)
    reviewer_attempt = dict(auto_attempt, attempt_id="attempt-" + reviewer_call,
        refs=dict(auto_attempt["refs"], panel_room_participant=reviewer_call,
                  panel_role="reviewer", panel_purpose="review", panel_access="read"))
    panel._prepare_dispatch_call(project, shared, reviewer_call, room_owner)
    if reviewer_label == "review-gate-guard-stop":
        reviewer_attempt.update(state="failed", reason_code="worker_authority_violation")
    review_record = {"attempt_id": reviewer_attempt["attempt_id"], "task_id": auto_task, "verify_exit": 0}
    with patch.object(panel_result_store, "_records", return_value=([reviewer_attempt], [review_record])), \
            patch.object(panel, "_dispatch_verifier", return_value=None), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": answer}]}]}), \
            patch.dict(os.environ, room_env, clear=True):
        panel.deliver_result(project, shared, reviewer_call, room_owner, auto_task, exit_status)
    verdict_mail = next(m for m in room.status(project, shared)["messages"]
                        if m["id"] == "result-" + reviewer_call)
    assert expected in verdict_mail["text"] and room_owner in verdict_mail["targets"], (reviewer_call, verdict_mail)
    if reviewer_label in {"review-gate-fail", "review-gate-last-fail",
                          "review-gate-fenced-suffix", "review-gate-fail-guard-text"}:
        assert "GATE: fail" in verdict_mail["text"]
    if reviewer_label == "review-gate-fail-guard-text":
        assert "guard stopped the review" not in verdict_mail["text"]
for dispatched_role in ("verifier", "security-auditor", "critic", "architect"):
    judge_call = fixture_call_id("judge-no-gate-" + dispatched_role)
    judge_attempt = dict(auto_attempt, attempt_id="attempt-" + judge_call,
        refs=dict(auto_attempt["refs"], panel_room_participant=judge_call,
                  panel_role="reviewer" if dispatched_role in {"verifier", "security-auditor"} else "advisor",
                  panel_purpose=dispatched_role, panel_access="read"))
    room.join(project, shared, judge_call, "codex",
              "reviewer" if dispatched_role in {"verifier", "security-auditor"} else "advisor",
              parent=room_owner)
    panel._prepare_dispatch_call(project, shared, judge_call, room_owner)
    with patch.object(panel_result_store, "_records", return_value=([judge_attempt], [])), \
            patch.object(panel, "_dispatch_verifier", return_value=None), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Findings checked."}]}]}), \
            patch.dict(os.environ, room_env, clear=True):
        panel.deliver_result(project, shared, judge_call, room_owner, auto_task, 0)
    judge_mail = next(m for m in room.status(project, shared)["messages"]
                      if m["id"] == "result-" + judge_call)
    assert "findings are ready for owner judgment" in judge_mail["text"]
    assert "review failed or is inconclusive" not in judge_mail["text"]
    assert "awaiting admission" not in judge_mail["text"] and room_owner in judge_mail["targets"]
# Completed negative automatic gates draft fixes for the main without dispatching another worker.
draft_main = {"attempt_id": "attempt-fix-draft-main", "tool": "panel-main", "provider": "claude",
              "state": "working", "terminal": False, "created_at": "2026-10-14T00:00:00Z",
              "refs": {"panel_role": "main", "panel_owner": "claude", "panel_room_id": shared,
                       "panel_room_participant": room_owner}}
original_fix_brief = "# Task\n\nRepair scripts/fixture.py only.\n\n# Constraints\n\nNo commits or dependency changes.\n"
fix_verify = "python3 -c 'print(\"it works\")'"
fix_execution = Path.home() / "oms-fixture-review-worktree"
fix_candidate = Path.home() / "oms-fixture-candidate-tmp" / "tmp.candidate"
fix_finding_blocks = ("- [P0] Validate `%s/scripts/fixture.py:7`.\n  Keep the caller contract." % project,
                      "- **P1** Refuse the unsafe path in `%s/scripts/fixture.py:9`.\n"
                      "  ```python\nP0: example within the finding\n  ```" % fix_execution,
                      "### P2 Preserve the verifier in `%s/scripts/fixture.py:11`." % fix_candidate,
                      "### P2 Keep this temporary checkout path repo-relative: `/tmp/tmp.X/scripts/fixture.py:13`.",
                      "### P2 Keep the doubled slash in `//scripts/fixture.py:15` apart from docs/a/b.md and https://x.y/z.",
                      "P3: Keep the manual dispatch step.")
fix_findings = "\n".join(fix_finding_blocks)
original_fix_notice = panel._review_fix_notice
for case_number, (case, expected_draft) in enumerate((
        ("failed-gate", True), ("round-two", True), ("round-three", True),
        ("truncated-summary", True), ("multiline-verify", True),
        ("notice-limit", True), ("notice-exact-limit", True),
        ("private-verifier", False),
        ("missing-candidate", False), ("wrong-candidate-digest", False),
        ("unsafe-candidate-root", False), ("relative-candidate-root", False),
        ("no-priorities", False), ("passed-gate", False), ("passed-drift", False), ("missing-context", False),
        ("bad-digest", False), ("private-path", False), ("stale-review", False),
        ("failed-provider", False), ("failed-verifier", False), ("oversized", False))):
    fix_task = "fix-draft-" + case
    if case.startswith("notice-"):
        fix_task = fix_task.ljust(160, "x")
    fix_worker = "call-%016x" % (0xD400 + case_number)
    claim_id, _, fix_reviewer = room.review_ids(fix_worker, auto_digest, 1)
    for ident, kind in ((fix_worker, "worker"), (fix_reviewer, "reviewer")):
        room.join(project, shared, ident, "codex", kind, parent=room_owner)
    panel._prepare_dispatch_call(project, shared, fix_reviewer, room_owner)
    worker_attempt = dict(auto_attempt, attempt_id="attempt-" + fix_worker, task_id=fix_task,
        sequence=3, state="review", parent_attempt_id=draft_main["attempt_id"],
        created_at="2026-10-14T00:00:03Z",
        refs=dict(auto_attempt["refs"], panel_room_participant=fix_worker, panel_owner="claude"))
    reviewer_attempt = dict(worker_attempt, attempt_id="attempt-" + fix_reviewer,
        refs=dict(worker_attempt["refs"], panel_room_participant=fix_reviewer, panel_role="reviewer",
                  panel_access="read", panel_location="worktree:oms-fixture-candidate-tmp/tmp.candidate"))
    fix_history = [draft_main]
    if case in {"round-two", "round-three"}:
        rounds = 2 if case == "round-two" else 3
        for round_index in range(1, rounds):
            fix_history.append(dict(worker_attempt,
                attempt_id="attempt-%s-prior-%d" % (fix_worker, round_index),
                created_at="2026-10-14T00:00:%02dZ" % round_index,
                refs=dict(worker_attempt["refs"], panel_room_participant="call-prior-%016x" % round_index)))
    fix_history.extend([worker_attempt, reviewer_attempt])
    if case == "stale-review":
        fix_history.append(dict(worker_attempt, attempt_id="attempt-later-worker", sequence=4,
                                refs=dict(worker_attempt["refs"], panel_room_participant="later-worker")))
    verifier = fix_verify if case != "multiline-verify" else "printf first\nprintf second"
    if case == "private-verifier":
        verifier = "test -f " + str(fix_execution) + "/scripts/fixture.py"
    metadata = dict(role="builder", scope=["scripts/fixture.py"], workload="routine", brief=original_fix_brief)
    panel._prepare_dispatch_call(project, shared, fix_worker, room_owner, verifier, fix_execution,
                                 None if case == "missing-context" else metadata)
    room.append(project, shared, {"kind": "review-dispatch", "event_id": claim_id, "task_id": fix_task,
        "call_id": fix_worker, "review_call_id": fix_reviewer, "patch_sha256": auto_digest, "attempt": 1},
        "Automatic review fixture")
    review_answer = fix_findings
    if case == "no-priorities":
        review_answer = "No parseable priorities."
    if case == "private-path":
        review_answer += "\n  Read %s." % (Path.home() / "oms-unrelated-private.txt")
    if case in {"truncated-summary", "oversized"}:
        review_answer += "\n  " + "complete evidence " * (2300 if case == "truncated-summary" else 5000)
    gate = "pass" if case in {"passed-gate", "passed-drift"} else "fail"
    review_answer = "> P0: quoted example\n```\nP1: fenced example\n```\n" + review_answer + "\nGATE: " + gate
    review_relative = ".oms/artifacts/fixture/review-" + case + ".md"
    review_file = project / review_relative
    review_file.write_text("## Output\n" + review_answer + "\n## Exit\n" +
                           ("1" if case == "failed-provider" else "0") + "\n", encoding="utf-8")
    review_index = {"kind": "review", "task_id": fix_task, "attempt_id": reviewer_attempt["attempt_id"],
                    "artifact": review_relative, "artifact_sha256": hashlib.sha256(review_file.read_bytes()).hexdigest(),
                    "exit": 0}
    if case == "bad-digest":
        review_index["artifact_sha256"] = "0" * 64
    # The real peer-review controller returns 1 for a completed GATE: fail and writes an outcome artifact
    # after the provider artifact. Provider failures and verifier failures still require manual judgment.
    outcome_index = dict(review_index, kind="review-outcome", exit=0 if gate == "pass" else 1,
                         review={"gate_verify_exit": 1 if case == "failed-verifier" else 0,
                                 "candidate": {"root": str(fix_candidate), "patch_sha256": auto_digest},
                                 "seats": [{"verdict": gate, "artifact": Path(review_relative).name}]})
    if case == "missing-candidate":
        outcome_index["review"].pop("candidate")
    elif case == "wrong-candidate-digest":
        outcome_index["review"]["candidate"]["patch_sha256"] = "0" * 64
    elif case == "unsafe-candidate-root":
        outcome_index["review"]["candidate"]["root"] = fix_candidate.anchor
    elif case == "relative-candidate-root":
        outcome_index["review"]["candidate"]["root"] = "tmp.candidate"
    projected_answer = "Retained prefix without the verdict" if case == "truncated-summary" else review_answer
    fix_products = [dict(auto_index, attempt_id=a["attempt_id"], task_id=fix_task, exit=0)
                    for a in fix_history if a.get("refs", {}).get("panel_access") == "write"]
    def notice_at_limit(*args, **kwargs):
        notice = original_fix_notice(*args, **kwargs)
        if case.startswith("notice-"):
            pointer = " Read findings: oms panel --results --task-id " + fix_task
            size = 3800 if case == "notice-limit" else 4000 - len(pointer.encode("utf-8"))
            notice = "검" * (size // 3) + "x" * (size % 3)
        return notice
    with patch.object(panel_result_store, "_records", return_value=(fix_history, fix_products + [review_index, outcome_index])), \
            patch.object(panel_result_store, "verified_patch", return_value=auto_patch), \
            patch.object(panel_result_store, "_worker_owns", return_value=["docs/fixture.py"] if case == "passed-drift" else ["scripts/fixture.py"]), \
            patch.object(panel, "_review_fix_notice", side_effect=notice_at_limit), \
            patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": projected_answer,
                "answer_truncated": case == "truncated-summary"}]}]}), \
            patch.object(panel, "dispatch") as draft_dispatch, patch.dict(os.environ, dict(room_env, OMS_AGENT="claude"), clear=True):
        assert panel.deliver_result(project, shared, fix_reviewer, room_owner, fix_task, outcome_index["exit"])
        assert panel.deliver_result(project, shared, fix_reviewer, room_owner, fix_task, outcome_index["exit"])
        draft_dispatch.assert_not_called()
    draft_relative = str(Path(review_relative).with_suffix(".fix-brief.md"))
    draft_file = project / draft_relative
    draft_mail = [m for m in room.status(project, shared)["messages"] if m["id"] == "result-" + fix_reviewer]
    assert len(draft_mail) == 1 and room_owner in draft_mail[0]["targets"], draft_mail
    assert draft_mail[0]["message_kind"] == "handoff" and room_owner in draft_mail[0]["pending_for"]
    if case == "failed-gate":
        assert "(review round 1)" in draft_mail[0]["text"]
    assert fix_reviewer in room.status(project, shared)["call_results"]
    assert not any(m["id"] == "judgment-" + fix_reviewer for m in room.status(project, shared)["messages"])
    assert draft_file.exists() == expected_draft, (case, draft_mail)
    if case.startswith("notice-"):
        delivered = draft_mail[0]["text"]
        assert 3998 <= len(delivered.encode("utf-8")) <= 4000, delivered
        assert "oms panel --results --task-id " + fix_task in delivered
        if case == "notice-limit":
            assert 3800 + len((" Read findings: oms panel --results --task-id " + fix_task).encode("utf-8")) > 4000
            assert "[Notice truncated.]" in delivered and review_relative in delivered
        else:
            assert len(delivered.encode("utf-8")) == 4000 and "truncated" not in delivered
        continue
    if expected_draft:
        draft_text = draft_file.read_text(encoding="utf-8")
        if case == "round-two":
            assert "(review round 2)" in draft_mail[0]["text"]
            assert "## Design check before another fix round" not in draft_text
            assert "consider whether the design is the problem" not in draft_mail[0]["text"]
        elif case == "round-three":
            assert "(review round 3)" in draft_mail[0]["text"]
            assert "## Design check before another fix round" in draft_text
            assert "the owner checks whether the design is the problem" in draft_text
            assert "consider whether the design is the problem" in draft_mail[0]["text"]
        assert fix_task in draft_text and original_fix_brief in draft_text, draft_text
        for finding in fix_finding_blocks:
            relative_finding = (finding.replace(str(project) + "/", "").replace(str(fix_execution) + "/", "")
                               .replace(str(fix_candidate) + "/", "").replace("/tmp/tmp.X/", ""))
            assert relative_finding in draft_text, relative_finding
        assert "Declared scope: scripts/fixture.py" in draft_text and verifier in draft_text
        assert "Fix every finding; same scope and authority as the previous round." in draft_text
        assert str(Path.home()) + "/" not in draft_text and str(project) not in draft_text
        assert "quoted example" not in draft_text and "fenced example" not in draft_text
        assert "P0: example within the finding" in draft_text
        assert "\n" not in draft_mail[0]["text"], draft_mail
        command = draft_mail[0]["text"].split("manual command: ", 1)[1].split(" Read findings: ", 1)[0]
        capture = subprocess.run(["bash", "-c", "oms() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' \"$@\"; }; " + command],
                                 capture_output=True, text=True, check=True)
        assert json.loads(capture.stdout) == ["panel", "--dispatch", "builder", "--continue", fix_task,
                                              "--brief-file", draft_relative, "--verify", verifier,
                                              "--access", "write", "--scope", "scripts/fixture.py"], command
        if case == "truncated-summary":
            assert "complete evidence " * 2300 in draft_text
        assert draft_file.stat().st_mode & 0o077 == 0
    if case == "passed-drift":
        assert "scripts/fixture.py" in draft_mail[0]["text"] and "1 path" in draft_mail[0]["text"], draft_mail
        assert "Acceptance will refuse" in draft_mail[0]["text"] and "--outcome accepted" not in draft_mail[0]["text"], draft_mail
        assert "finalize completed" in draft_mail[0]["text"] and "re-dispatch" in draft_mail[0]["text"], draft_mail
        with patch.object(panel_result_store, "_records", return_value=(fix_history, fix_products)), \
                patch.object(panel_result_store, "_worker_owns", return_value=["allowed/"]), \
                patch.object(panel, "_review_verifier", return_value=verifier), \
                patch.object(panel, "_write_brief_artifact"):
            context = {"call_id": fix_worker, "patch_sha256": auto_digest}
            def completion_for(data):
                with patch.object(panel_result_store, "verified_patch", return_value=data):
                    return panel._completion_inputs(project, shared, fix_reviewer, room_owner, fix_task,
                                                    {"artifact": review_relative}, context)
            baseline = completion_for(b"")
            assert "--outcome accepted" in baseline
            assert completion_for(b"diff --git malformed\n") == baseline
            for count in (8, 9):
                paths = ["outside/file%d" % i for i in range(count)]
                data = b"".join(("diff --git a/%s b/%s\n" % (p, p)).encode() for p in paths)
                result = completion_for(data)
                assert "%d path(s)" % count in result and all(p in result for p in paths[:8]), result
                assert (paths[-1] in result) == (count == 8) and "--outcome accepted" not in result
            with patch.object(panel_result_store, "verified_patch", side_effect=OSError("unavailable")):
                assert panel._completion_inputs(project, shared, fix_reviewer, room_owner, fix_task,
                                                {"artifact": review_relative}, context) == baseline
    if case == "passed-gate":
        completion = project / (".oms/artifacts/panel-results/completion-%s.md" % fix_reviewer)
        assert completion.is_file() and verifier in completion.read_text()
        assert "After admission" in draft_mail[0]["text"]
        owner_command = draft_mail[0]["text"].split("owner command: ", 1)[1].split(" Read findings: ", 1)[0]
        capture = subprocess.run(["bash", "-c", "oms() { python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' \"$@\"; }; " + owner_command],
                                 capture_output=True, text=True, check=True)
        owner_argv = json.loads(capture.stdout)
        assert owner_argv[:5] == ["panel", "--finalize", "--owner", "claude", "--task-id"], owner_argv
        assert owner_argv[5:] == [fix_task, "--summary-file", str(completion.relative_to(project)),
                                  "--outcome", "accepted", "--verify", verifier,
                                  "--evidence", owner_argv[-1]], owner_argv
        assert owner_argv[-1].startswith(".oms/artifacts/"), owner_argv
# Draft writes preserve existing evidence and reject links just like the call artifacts.
try:
    panel._write_brief_artifact(project, ".oms/artifacts/fixture/review-failed-gate.fix-brief.md",
                                "overwrite existing evidence")
except ValueError:
    pass
else:
    raise AssertionError("draft artifact was overwritten")
if os.name == "posix":
    draft_link = project / ".oms/artifacts/fixture/review-link.fix-brief.md"
    draft_link.symlink_to(review_file)
    link_target = review_file.read_bytes()
    try:
        panel._write_brief_artifact(project, str(draft_link.relative_to(project)), "follow a link")
    except ValueError:
        pass
    else:
        raise AssertionError("draft artifact followed a symlink")
    assert review_file.read_bytes() == link_target
# Two exhausted review reservations become a retryable manual decision until its notice is durable.
r5_call = "call-f5f5f5f5f5f5f5f5"
r5_attempt = dict(auto_attempt, attempt_id="attempt-review-reservations-exhausted",
    refs=dict(auto_attempt["refs"], panel_room_participant=r5_call))
r5_patch = auto_patch + b"# exhausted reservations\n"
r5_digest = hashlib.sha256(r5_patch).hexdigest()
room.join(project, shared, r5_call, "codex", "worker", parent=room_owner)
panel._prepare_dispatch_call(project, shared, r5_call, room_owner,
                             "test -f scripts/fixture.py", worker_execution_repo)
for number in (1, 2):
    claim_id, failed_id, review_call = room.review_ids(r5_call, r5_digest, number)
    reservation = {"kind": "review-dispatch", "event_id": claim_id,
                   "task_id": auto_task, "call_id": r5_call, "patch_sha256": r5_digest,
                   "review_call_id": review_call, "attempt": number}
    assert room.claim_review(project, shared, reservation)
    room.fail_review(project, shared, {"kind": "review-dispatch-failed",
        "event_id": failed_id,
        "task_id": auto_task, "call_id": r5_call, "patch_sha256": r5_digest,
        "review_call_id": review_call, "attempt": number})
r5_notice_failures = []
real_room_send = room.send
def fail_r5_notice_once(*args, **kwargs):
    if kwargs.get("message_id") == "judgment-" + r5_call and not r5_notice_failures:
        r5_notice_failures.append(kwargs["message_id"])
        raise OSError("fixture exhausted-reservation notice unavailable")
    return real_room_send(*args, **kwargs)
with patch.object(panel_result_store, "_records", return_value=([r5_attempt], [dict(
        auto_index, attempt_id=r5_attempt["attempt_id"], patch_sha256=r5_digest)])), \
        patch.object(panel_result_store, "verified_patch", return_value=r5_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "work_repository", return_value=worker_execution_repo), \
        patch.object(panel.room, "send", side_effect=fail_r5_notice_once), \
        patch.dict(os.environ, room_env, clear=True):
    assert not panel.deliver_result(project, shared, r5_call, room_owner, auto_task, 0)
    r5_claim = panel.dispatch_store.read(project, panel._settlement_claim_path(r5_call))
    assert not r5_claim["complete"] and r5_claim["steps"]["auto_review"] is False, r5_claim
    assert panel.deliver_result(project, shared, r5_call, room_owner, auto_task, 0)
assert r5_notice_failures == ["judgment-" + r5_call]
assert panel.dispatch_store.read(project, panel._settlement_claim_path(r5_call))["complete"]
assert any(message.get("id") == "judgment-" + r5_call
           for message in room.status(project, shared)["messages"])
# Concurrent observers share one durable call claim and launch at most one reviewer per digest.
import threading
parallel_call = "call-f6f6f6f6f6f6f6f6"
parallel_attempt = dict(auto_attempt, attempt_id="attempt-settlement-parallel",
    refs=dict(auto_attempt["refs"], panel_room_participant=parallel_call))
parallel_patch = auto_patch + b"# concurrent settlement\n"
parallel_digest = hashlib.sha256(parallel_patch).hexdigest()
room.join(project, shared, parallel_call, "codex", "worker", parent=room_owner)
panel._prepare_dispatch_call(project, shared, parallel_call, room_owner,
                             "test -f scripts/fixture.py", worker_execution_repo)
parallel_barrier = threading.Barrier(4)
parallel_errors = []
def deliver_parallel():
    try:
        parallel_barrier.wait(timeout=5)
        panel.deliver_result(project, shared, parallel_call, room_owner, auto_task, 0)
    except BaseException as error:
        parallel_errors.append(error)
with patch.object(panel_result_store, "_records", return_value=([parallel_attempt], [dict(
        auto_index, attempt_id=parallel_attempt["attempt_id"], patch_sha256=parallel_digest)])), \
        patch.object(panel_result_store, "verified_patch", return_value=parallel_patch), \
        patch.object(panel, "_dispatch_verifier", return_value="test -f scripts/fixture.py"), \
        patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": "Worker finished."}]}]}), \
        patch.object(panel, "run_dispatch", side_effect=fixture_dispatch), \
        patch.object(panel, "work_repository", return_value=worker_execution_repo), \
        patch.object(panel, "provider_catalog", return_value=catalog_rows), \
        patch.dict(os.environ, room_env, clear=True):
    observers = [threading.Thread(target=deliver_parallel) for _ in range(4)]
    for observer in observers:
        observer.start()
    import time
    join_deadline = time.monotonic() + max(15, int(os.environ.get("OMS_TEST_CALL_TIMEOUT", "90")) // 6)
    for observer in observers:
        observer.join(timeout=max(0.0, join_deadline - time.monotonic()))
assert not parallel_errors and not any(observer.is_alive() for observer in observers), parallel_errors
parallel_events = [row["room_event"] for row in room.records(project, shared)
                   if row.get("room_event", {}).get("kind") == "call-result"
                   and row["room_event"].get("call_id") == parallel_call]
parallel_messages = [message for message in room.status(project, shared)["messages"]
                     if message.get("id") == "result-" + parallel_call]
assert len(parallel_events) == len(parallel_messages) == 1, (parallel_events, parallel_messages)
assert panel.dispatch_store.read(project, panel._settlement_claim_path(parallel_call))["complete"]

# Repeated tool hooks remind about delivered mail only once per interval, and never acknowledge it.
import hook_state
ack_room = room.create(project, "ack-reminder-fixture", "Acknowledgment throttle")
room.join(project, ack_room, "ack-sender", "codex", "main")
room.join(project, ack_room, "ack-reader", "claude", "reviewer", native_session="panel-ack-fixture")
room.send(project, ack_room, "ack-sender", "ack-reader", "Review result ready.", message_id="ack-mail")
ack_payload = {"cwd": str(project), "session_id": "panel-ack-fixture", "hook_event_name": "PostToolUse"}
with patch.dict(os.environ, dict(environment, OMS_ROOM_ID=ack_room, OMS_ROOM_REPO=str(project),
                                OMS_ROOM_PARTICIPANT="ack-reader"), clear=True):
    delivered = hook_state.live_thread_hint(ack_payload, project)
    assert "ack-mail" in delivered and "explicitly ack" in delivered
    later = room.now() + room.ACK_AFTER + 1
    with patch.object(room, "now", return_value=later):
        assert "delivered message" in hook_state.live_thread_hint(ack_payload, project)
        assert not hook_state.live_thread_hint(ack_payload, project)
        assert room.status(project, ack_room)["pending_count"] == 1
    with patch.object(room, "now", return_value=later + room.ACK_AFTER + 1):
        assert "delivered message" in hook_state.live_thread_hint(ack_payload, project)
    room.acknowledge(project, ack_room, "ack-reader", ["ack-mail"])
    assert not hook_state.live_thread_hint(ack_payload, project)

# Mechanical review sees base + patch, even when the original checkout still fails the verifier.
source_value = (project / "value.txt").read_bytes()
candidate_base = subprocess.check_output(["git", "-C", str(project), "rev-parse", "HEAD"], text=True).strip()
(project / "value.txt").write_text("candidate-only\n")
candidate_patch = subprocess.check_output(["git", "-C", str(project), "diff", "--binary", "HEAD", "--", "value.txt"])
(project / "value.txt").write_bytes(source_value)
candidate_patch += (b"diff --git a/tests/candidate-check.sh b/tests/candidate-check.sh\nnew file mode 100644\n"
                    b"--- /dev/null\n+++ b/tests/candidate-check.sh\n@@ -0,0 +1,2 @@\n"
                    b"+#!/usr/bin/env bash\n+grep -qx candidate-only value.txt\n")
candidate_relative = ".oms/artifacts/fixture/candidate.patch"
(project / candidate_relative).write_bytes(candidate_patch)
request = ["bash", str(root / "scripts/peer-review.sh"), "--repo", str(project), "--providers", "codex",
           "--model", "gpt-6-luna", "--gate", "--prompt", "Review the complete candidate diff.",
           "--candidate-patch", candidate_relative, "--candidate-base", candidate_base,
           "--candidate-sha256", hashlib.sha256(candidate_patch).hexdigest(), "--verify",
           "bash tests/candidate-check.sh"]
candidate_env = dict(room_env, OMS_PANEL_DISPATCH="1", OMS_PANEL_ROLE="reviewer", OMS_PANEL_PURPOSE="review",
                     OMS_TASK_ID="candidate-gate")
candidate_temp = home / "review candidate tmp"
candidate_temp.mkdir()
candidate_temp_alias = temporary / "review candidate tmp alias"
if os.name == "posix":
    candidate_temp_alias.symlink_to(candidate_temp, target_is_directory=True)
else:
    candidate_temp_alias = candidate_temp
candidate_env["TMPDIR"] = str(candidate_temp_alias)
assert subprocess.run(["grep", "-qx", "candidate-only", "value.txt"], cwd=project).returncode == 1
candidate_run = subprocess.run(request, env=candidate_env, capture_output=True, text=True, timeout=90)
assert candidate_run.returncode == 0 and "gate verify: pass" in candidate_run.stdout, candidate_run
assert (project / "value.txt").read_bytes() == source_value
review_dir = project / ".oms/artifacts/review"
scope = json.loads(max(review_dir.glob("_scope-*.json"), key=lambda p: p.stat().st_mtime_ns).read_text())
assert scope["paths"] == ["tests/candidate-check.sh", "value.txt"] and scope["base"] == candidate_base and scope["diff_sha256"]
assert scope["candidate"]["patch_sha256"] == hashlib.sha256(candidate_patch).hexdigest() and scope["candidate"]["tree"]
candidate_root = Path(scope["candidate"]["root"])
assert candidate_root.is_absolute() and candidate_root.parent == candidate_temp.resolve(), scope
assert not candidate_root.exists(), "candidate metadata must survive checkout cleanup"
candidate_outcomes = [row for row in panel_result_store._records(project, full=True)[1]
                      if row.get("kind") == "review-outcome" and row.get("task_id") == "candidate-gate"]
assert candidate_outcomes[-1]["review"]["candidate"] == scope["candidate"], candidate_outcomes
before_bad_gate = worker_log.read_bytes()
for invalid in (request[:-1] + ["printf tampered > value.txt"],
                ["0" * 64 if arg == hashlib.sha256(candidate_patch).hexdigest() else arg for arg in request]):
    rejected_gate = subprocess.run(invalid, env=candidate_env, capture_output=True, text=True, timeout=90)
    assert rejected_gate.returncode != 0 and "candidate" in rejected_gate.stderr, rejected_gate
    assert worker_log.read_bytes() == before_bad_gate and (project / "value.txt").read_bytes() == source_value
shared, room_env = shared_before_auto, room_env_before_auto
