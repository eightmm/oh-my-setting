def exercise_observed_worker_leases():
    # Every success marker comes from the real peer dispatcher while the fake CLI waits.
    import time
    codex_candidate = {"provider": "codex", "binary": "codex", "installed": True,
                       "models": ["gpt-6-sol"], "catalog_models": ["gpt-6-sol", "gpt-6.2-sol"],
                       "access": ["read", "write"], "exact_model_override": True}
    claude_candidate = {"provider": "claude", "binary": "claude", "installed": True, "models": [],
                        "access": ["read", "write"], "exact_model_override": True}
    def fixture_selected_route(*args, **kwargs):
        target = kwargs.get("target", args[5] if len(args) > 5 else None)
        catalog = [row for row in (codex_candidate, claude_candidate)
                   if target in (None, row["provider"])]
        with patch.object(panel, "provider_catalog", return_value=catalog):
            return panel.selected_route(*args, **kwargs)
    codex_only = {"provider": "codex", "binary": "codex", "installed": True, "models": ["gpt-6-sol"],
                  "catalog_models": ["gpt-6-sol", "gpt-6.1-sol", "gpt-6.2-sol", "gpt-7-astra"],
                  "access": ["read", "write"], "exact_model_override": True}
    plan_api = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(project)]
    # The observer tuple belongs on the peer dispatcher argv; the native CLI
    # receives ordinary model flags and cannot expose this frontend argument.
    argv_binding = panel.PlanBinding("plan_" + "1" * 32, "argv-observer", "lease_" + "2" * 32,
                                     "codex", "claude", "argv-main", "att_" + "3" * 32, "argv-room")
    captured_dispatch = {}
    route = fixture_selected_route("claude", "worker", "light", "write", "implement")
    dispatch_args = (project, "claude", "worker", "write",
                     None, brief, "--task-id", "argv-observer", False, None, route, None, None, None)
    # The fake provider CLIs live in the fixture PATH, but this in-process
    # observer check inherits the host PATH. Keep route availability explicit
    # so CI hosts without Codex or Claude still exercise the real dispatcher.
    with patch.object(panel.shutil, "which", return_value=None), \
            patch.object(panel.subprocess, "Popen") as unavailable_call:
        try:
            panel.run_dispatch(*dispatch_args)
        except ValueError as error:
            assert "%s is not installed" % route["provider"] in str(error), error
        else:
            raise AssertionError("unavailable worker route was accepted")
        unavailable_call.assert_not_called()
    with patch.object(panel.shutil, "which", return_value=str(binary / route.get("binary", route["provider"]))), \
            patch.object(panel, "moved_main", return_value=None), \
            patch.object(panel, "child_environment", return_value={}), \
            patch.object(panel, "native_repository", return_value=project), \
            patch.object(panel, "plan_link", return_value=argv_binding), \
            patch.multiple(panel, _dispatch_descendants=Mock(return_value=[]), \
                           _dispatch_call_finished=Mock(return_value=True)), \
            patch.object(panel.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "", "")) as preflight, \
            patch.object(panel.subprocess, "Popen", side_effect=lambda argv, **kwargs:
                         captured_dispatch.update(argv=argv) or Mock(pid=0, wait=lambda **kwargs: 0, poll=lambda: 0)):
        assert panel.run_dispatch(*dispatch_args) == 0
    preview_argv = next(call.args[0] for call in preflight.call_args_list if call.args and call.args[0][-1:] == ["--dry-run"])
    assert "--observe-plan-binding" not in preview_argv
    assert preview_argv[preview_argv.index("--verify") + 1] == "--task-id"
    assert preview_argv[-3] == "--task-id" and preview_argv[-2].startswith("preflight-")
    dispatch_argv = captured_dispatch["argv"]
    assert "--observe-plan-binding" in dispatch_argv, dispatch_argv
    assert json.loads(dispatch_argv[dispatch_argv.index("--observe-plan-binding") + 1]) == argv_binding._asdict()
    # Capacity refusals leave enrollment and plan claims untouched; assignment/spawn failures roll enrollment back.
    import io
    from contextlib import redirect_stderr
    capacity_route = fixture_selected_route("claude", "worker", "light", "read", "investigate")
    capacity_env = {"OMS_ROOM_ID": argv_binding.room_id,
                    "OMS_ROOM_PARTICIPANT": argv_binding.owner_participant,
                    "OMS_PANEL_MAIN_ATTEMPT": argv_binding.owner_attempt}
    capacity_args = (project, "claude", "worker", "read",
                     "Capacity fixture", None, None, argv_binding.task_id, False,
                     "Capacity call", capacity_route, None, None, None)
    with patch.object(panel, "provider_catalog", return_value=[codex_only, claude_candidate]):
        capacity_write_route = panel.selected_route("claude", "worker", "light", "write", "implement")
    for scenario in ("normal", "full", "full-write", "calls-full", "calls-stuck", "closed", "foreign-main",
                     "lost-caller", "lost-child", "send-error", "spawn-error"):
        writing = scenario in {"full-write", "spawn-error"}
        scopes = ["value.txt"] if writing else None
        this_args = capacity_args if not writing else (
            project, "claude", "worker", "write", None,
            brief, "git diff --check", argv_binding.task_id, False, "Capacity call",
            fixture_selected_route("claude", "worker", "light", "write", "implement"), None, None, scopes)
        state = {"messages": [{}] * (room.MAX_MESSAGES - 1 if scenario in {"full", "full-write"} else 1),
                 "closed": scenario == "closed", "participants": [{
                     "participant": argv_binding.owner_participant, "role": "main",
                     "provider": "claude", "joined": True}]}
        if scenario in {"calls-full", "calls-stuck"}:
            state["participants"] += [{"participant": "full-%d" % i, "parent": argv_binding.owner_participant,
                                       "joined": False} for i in range(room.MAX_CALLS)]
        participants_before = len(state["participants"])
        main_record = {"tool": "panel-main", "provider": "claude", "terminal": False,
                       "attempt_id": argv_binding.owner_attempt, "state": "working",
                       "refs": {"panel_role": "main", "panel_room_participant": argv_binding.owner_participant,
                                "panel_room_id": "foreign-room" if scenario == "foreign-main" else argv_binding.room_id}}
        notices, children, leaves = [], [], []
        def rollback(repo, room_id, member):
            event = {"kind": "leave", "participant": member}
            leaves.append(event)
            next((participant for participant in state["participants"]
                  if participant["participant"] == event.get("participant")), {}).update(joined=False)
        live_rows = [{"parent_attempt_id": argv_binding.owner_attempt, "terminal": False,
                      "refs": {"panel_role": "worker"}} for _ in range(5)] if scenario == "normal" else []
        # Live calls of this main or of others never block a dispatch: a main has no call cap.
        live_rows += [{"parent_attempt_id": "other-main", "terminal": False, "refs": {"panel_role": "worker"}},
                      {"parent_attempt_id": argv_binding.owner_attempt, "terminal": True,
                       "refs": {"panel_role": "worker"}}]
        def enroll(repo, room_id, member, provider, role, model, label, owns, parent, **_join_fields):
            children.append((room_id, member, provider, role, model, owns, parent))
            state["participants"].append({"participant": member, "provider": provider,
                                           "role": role, "joined": True})
            if scenario in {"lost-caller", "lost-child"}:
                state["participants"].pop(0 if scenario == "lost-caller" else -1)
        rolled = room.successor_id(argv_binding.room_id)
        def roll(repo, room_id, actor):
            # A room past its marks continues as its successor; a refused carry leaves the capacity refusal.
            assert (room_id, actor) == (argv_binding.room_id, argv_binding.owner_participant)
            if scenario in {"full-write", "calls-stuck"}:
                raise ValueError("rollover carry exceeds reserve")
            state["messages"] = [{}]
            state["participants"] = [p for p in state["participants"] if not p.get("parent")]
            return rolled
        def notice(repo, room_id, sender, recipient, text, kind, **kwargs):
            if scenario == "send-error" and text.startswith("Assigned:"):
                raise ValueError("unrelated room send failure")
            if len(state["messages"]) >= 1024:
                raise ValueError("room message limit reached; start a new bounded room")
            notices.append((sender, recipient, text))
        errors = io.StringIO()
        rolling = Mock(side_effect=roll)
        with patch.object(panel.shutil, "which", return_value="fixture-codex"), \
                patch.object(panel, "moved_main", return_value=None), \
                patch.object(panel, "child_environment", side_effect=lambda *args: dict(capacity_env)), \
                patch.object(panel, "native_repository", return_value=project), \
                patch.object(panel, "events", side_effect=lambda repo, operation, *args, **kwargs:
                             json.dumps(main_record if operation == "show" else live_rows)), \
                patch.object(panel, "main_history", return_value=[main_record]), \
                patch.object(panel, "plan_link", return_value=argv_binding) as observed_plan, \
                patch.object(panel, "_rollback_dispatch_enrollment", side_effect=rollback), \
                patch.multiple(room, records=Mock(return_value=[]), project=Mock(return_value=state),
                               join=Mock(side_effect=enroll), rollover=rolling, send=Mock(side_effect=notice)), \
                patch.object(panel, "results", return_value={"rows": []}), \
                patch.object(panel.subprocess, "run", side_effect=lambda argv, **kwargs:
                             subprocess.CompletedProcess(argv, 0,
                                 json.dumps([{"attempt_id": "fixture-capacity-result", "refs": {
                                     "panel_room_id": argv_binding.room_id,
                                     "panel_room_participant": next(p["participant"] for p in state["participants"]
                                                                     if p.get("role") == "worker"),
                                     "panel_role": "worker", "panel_access": "write" if writing else "read"}}]) if "agent-events" in argv
                                 else '{"rows":[]}' if "artifact-index" in argv else "", "")), \
                patch("panel_terminals.process", return_value=None), \
                patch.multiple(panel, _dispatch_group_gone=Mock(return_value=True), \
                               _dispatch_descendants=Mock(return_value=[]), \
                               _dispatch_call_finished=Mock(return_value=True), _settle_dispatch=Mock()), \
                patch.object(panel.subprocess, "Popen", side_effect=OSError("fixture spawn failure")
                             if scenario == "spawn-error" else None,
                             return_value=Mock(pid=123, wait=lambda **kwargs: 0, poll=lambda: 0)) \
                    as provider_call, redirect_stderr(errors):
            if scenario not in {"normal", "full", "calls-full"}:
                try:
                    panel.run_dispatch(*this_args)
                except (OSError, ValueError) as error:
                    expected = {"closed": "room is closed", "foreign-main": "identities disagree",
                                "full-write": "room %s is full" % argv_binding.room_id,
                                "calls-stuck": "room %s is full" % argv_binding.room_id,
                                "lost-caller": "participant has not joined this room",
                                "lost-child": "participant has not joined this room",
                                "send-error": "unrelated room send failure",
                                "spawn-error": "fixture spawn failure"}[scenario]
                    assert expected in str(error), (scenario, error)
                else:
                    raise AssertionError("invalid capacity dispatch was accepted: " + scenario)
                if scenario in {"send-error", "spawn-error"}:
                    assert not next(p for p in state["participants"] if p["participant"] == children[0][1])["joined"], scenario
                    assert leaves == [{"kind": "leave", "participant": children[0][1]}]
                if scenario == "spawn-error":
                    provider_call.assert_called_once()
                    assert children[0][5] == ["value.txt"]
                    assert "owner cleanup is required" not in errors.getvalue()
                    assert "scope retained" not in errors.getvalue()
                    assert not (project / panel._dispatch_pending_path(children[0][1])).exists()
                else:
                    provider_call.assert_not_called()
                if scenario in {"full-write", "calls-stuck", "closed"}:
                    assert len(state["participants"]) == participants_before and not children
                    observed_plan.assert_not_called()
                if scenario in {"full-write", "calls-stuck"}:
                    assert "could not roll over: rollover carry exceeds reserve" in errors.getvalue()
                continue
            assert panel.run_dispatch(*this_args) == 0, scenario
            provider_call.assert_called_once()
            assert provider_call.call_args.kwargs["start_new_session"] == (os.name == "posix")
            invoked, child_env = provider_call.call_args.args[0], provider_call.call_args.kwargs["env"]
            assert json.loads(invoked[invoked.index("--observe-plan-binding") + 1]) == argv_binding._asdict()
            assert capacity_route["model"] in " ".join(invoked), invoked
            # After a rollover the call enrolls in, and its worker is told, the successor room.
            current = rolled if scenario != "normal" else argv_binding.room_id
            assert rolling.called == (scenario != "normal"), scenario
            assert child_env["OMS_ROOM_ID"] == current
            assert child_env["OMS_PANEL_MAIN_ATTEMPT"] == argv_binding.owner_attempt
            assert child_env["OMS_ROOM_PARTICIPANT"] == children[0][1]
            assert children[0][0] == current and children[0][-1] == argv_binding.owner_participant
            assert children[0][2:6] == (capacity_route["provider"], "worker", capacity_route["model"], scopes)
            assert child_env["OMS_PANEL_PURPOSE"] == ("implement" if writing else "investigate")
            assert notices[0][2] == "Assigned: Capacity call"
            assert errors.getvalue() == ("" if scenario == "normal" else "Room %s continues as %s; ids naming %s keep working.\n"
                                         % (argv_binding.room_id, rolled, argv_binding.room_id)), errors.getvalue()
    test_rooms = {}
    mains = {}
    for provider in panel.PROVIDERS:
        test_room = room.create(project, "observed-worker-room-" + provider, "Worker lease producer")
        participant = "observed-main-" + provider
        attempt = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                        "--provider", provider, "--tool", "panel-main", "--ref", "panel_role=main",
                        "--ref", "panel_room_id=" + test_room, "--ref", "panel_room_participant=" + participant,
                        "--then", "starting", "--then", "working"]).strip()
        legacy_main_join(project, test_room, participant, provider, "main")
        mains[provider] = participant, attempt
        test_rooms[provider] = test_room
    source = Path(call(["bash", str(panel.ENTRY), "scratch-worktree", "add", "--repo", str(project),
                        "--owner-pid", str(os.getpid())]).strip())
    (source / "value.txt").write_text("lease fixture scratch source\n")
    subprocess.run(["git", "-C", str(source), "add", "value.txt"], check=True)
    subprocess.run(["git", "-C", str(source), "-c", "core.hooksPath=/dev/null", "commit", "-qm", "fixture scratch"], check=True)
    source_sha = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
    primary_sha = subprocess.check_output(["git", "-C", str(project), "rev-parse", "HEAD"], text=True).strip()
    assert source_sha != primary_sha
    print("scratch source HEAD:", source_sha, "canonical HEAD:", primary_sha)
    scenarios = [("codex", "routine", "codex", "gpt-6-luna", "medium", "write", True),
                 ("claude", "light", "codex", "gpt-6-luna", "low", "write", True),
                 ("codex", "routine", "codex", "gpt-6-luna", "medium", "write", False),
                 ("claude", "light", "codex", "gpt-6-luna", "low", "read", True),
                 ("codex", None, "codex", "gpt-6-luna", "low", "read", True, "researcher")]
    for number, scenario in enumerate(scenarios):
        owner, workload, worker, model, effort, access, reviewed = scenario[:7]
        test_room = test_rooms[owner]
        role = scenario[7] if len(scenario) > 7 else "worker"
        purpose = "research" if role == "researcher" else "implement" if access == "write" else "investigate"
        task_id = "observed-worker-" + str(number)
        main_member, main_attempt = mains[owner]
        assignment = dict(provider=worker, model=model, reasoning_effort=effort) if reviewed else {}
        call(plan_api + ["init", "--goal", "Observed worker fixture"])
        call(plan_api + ["add", "--id", task_id, "--title", "Exact observed worker", "--assignment", json.dumps(assignment)])
        entered, release = temporary / (task_id + "-entered"), temporary / (task_id + "-release")
        env = dict(environment, OMS_ROOM_ID=test_room, OMS_ROOM_REPO=str(project), OMS_ROOM_PARTICIPANT=main_member,
                   OMS_PANEL_MAIN_ATTEMPT=main_attempt, OMS_LOCK_DIR=str(temporary / "observed-locks"),
                   OMS_PLAN_LEASE_ID="ambient-not-authority", OMS_OBSERVER_INJECTED="untrusted", OMS_DL_OBSERVATION="untrusted", OMS_INDEX_BASE_REPO="untrusted", DELEGATE_SOURCE_REPO="untrusted",
                   LEASE_FIXTURE_ENTERED=str(entered), LEASE_FIXTURE_RELEASE=str(release),
                   LEASE_FIXTURE_MODEL=model, LEASE_FIXTURE_EFFORT=effort, LEASE_FIXTURE_PROVIDER=worker,
                   OMS_PEER_TIMEOUT="90")
        if access == "read":
            # Read dispatch observes a lease owned by the main; it does not
            # claim a ready plan task on the observer's behalf.
            call(plan_api + ["claim", "--id", task_id, "--provider", worker], env)
        argv = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
                "--dispatch", role, "--owner", owner, "--access", access]
        argv += ["--to", worker, "--model", model, "--reasoning-effort", effort]
        if role == "researcher":
            argv += ["--prompt", "Inspect the native source read-only."]
        else:
            argv += ["--purpose", purpose, "--brief-file", str(brief)]
        if access == "write":
            argv += ["--verify", ":"]
        argv += ["--task-id", task_id]
        child_process = subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            # Patience, not a contract: the fixture's own peer timeout is 90 s, and a loaded full gate
            # took longer than 30 s to start the child.
            deadline = time.monotonic() + 90
            while not entered.exists() and child_process.poll() is None and time.monotonic() < deadline:
                time.sleep(.05)
            if not entered.exists():
                output, errors = child_process.communicate(timeout=60)
                raise AssertionError((argv, child_process.returncode, output, errors))
            observed = json.loads(entered.read_text())
            row = json.loads(call(plan_api + ["evidence-snapshot", "--id", task_id], env))
            marker = next(json.loads(p.read_text()) for p in (project / ".oms/delegations").glob("*.json")
                          if json.loads(p.read_text())["task_id"] == task_id)
            assert marker["schema"] == 4 and marker["lease_id"] == row["lease_id"], marker
            assert (marker["provider"], marker["model"], marker["reasoning_effort"]) == (worker, model, effort), marker
            assert marker["native_pid"] > 0 and marker["native_pid_source"] and marker["state"] == "running", marker
            os.kill(marker["native_pid"], 0)
            assert observed["head"] == source_sha and observed["value"] == "lease fixture scratch source\n", observed
            assert observed["cwd"] == marker["worktree"] and observed["cwd"] != str(source), observed
            if role == "researcher":
                assert observed["role"] == "researcher" and observed["purpose"] == "research", observed
                native_argv = json.loads(worker_log.read_text().splitlines()[-1])["argv"]
                assert "--sandbox" in native_argv and native_argv[native_argv.index("--sandbox") + 1] == "read-only", native_argv
                assert "--verify" not in native_argv and "--read-only" not in native_argv, native_argv
            else:
                assert observed["role"] == "worker" and observed["purpose"] == purpose, observed
            assert not (source / ".oms/delegations").exists(), "state split into scratch"
            assert row["claimed_by_participant"] == main_member and row["assignment"] == assignment, row
            assert row["provider"] == (worker if reviewed else owner), row
            observed_binding = dict(plan_id=row["plan_id"], task_id=task_id, lease_id=row["lease_id"],
                                    claim_provider=row["provider"], owner_provider=owner,
                                    owner_participant=main_member, owner_attempt=main_attempt,
                                    room_id=test_room)
            if reviewed and access == "write" and worker != owner:
                assert observed_binding["claim_provider"] != observed_binding["owner_provider"], observed_binding
            enrolled = room.participant(room.project(room.records(project, test_room)), observed["participant"])
            assert enrolled["parent"] == main_member and observed["parent"] == main_attempt, enrolled
            assert enrolled["role"] == role and enrolled["owns"] == [], enrolled
            route = fixture_selected_route(owner, role, None, access, purpose if role == "worker" else None,
                                           worker, model, effort)
            validation_route = dict(provider=route["provider"], model=route["model"],
                                    reasoning_effort=route["effort"], fallback_model="")
            binding = panel.PlanBinding(row["plan_id"], task_id, row["lease_id"], row["provider"], owner,
                                        main_member, main_attempt, test_room)
            assert binding._asdict() == observed_binding, binding
            child_env = dict(env, OMS_AGENT=owner, OMS_TASK_ID=task_id,
                             OMS_ROOM_PARTICIPANT=observed["participant"],
                             OMS_ROOM_ADMITTED_PARTICIPANT=observed["participant"])
            if role == "researcher":
                child_env["OMS_PANEL_ROLE"] = role
                binding.validate(project, child_env, validation_route)
                room_log = project / ".oms/threads" / (test_room + ".jsonl")
                room_before = room_log.read_bytes()
                for bad_state in ({"joined": False}, {"joined": True, "role": "worker", "parent": main_member,
                                   "provider": validation_route["provider"], "model": validation_route["model"]},
                                  {"joined": True, "role": "researcher", "parent": main_member,
                                   "provider": validation_route["provider"], "model": "wrong-model"}):
                    with patch.object(panel.room, "participant", return_value=dict(enrolled, **bad_state)):
                        try:
                            binding.validate(project, child_env, validation_route)
                        except (ValueError, KeyError):
                            pass
                        else:
                            raise AssertionError("plan observer accepted invalid researcher membership: " + repr(bad_state))
                wrong_controller_role = dict(child_env, OMS_PANEL_ROLE="worker")
                try:
                    binding.validate(project, wrong_controller_role, validation_route)
                except ValueError:
                    pass
                else:
                    raise AssertionError("researcher observation accepted a worker controller role")
                assert room_log.read_bytes() == room_before, "observer validation changed room events"
            else:
                # Older worker observers have no controller role marker; the validator defaults to worker.
                binding.validate(project, child_env, validation_route)
            for lease_state in ("claimed", "running"):
                # Clock/state edits are adversarial private-fixture data, serialized by the production plan lock.
                mutate = ('import json,sys; from pathlib import Path; p=Path(sys.argv[1]); d=json.loads(p.read_text()); '
                          'd["tasks"][sys.argv[2]].update(state=sys.argv[3], claimed_at="2000-01-01T00:00:00Z",ttl="1"); '
                          'p.write_text(json.dumps(d))')
                call(["bash", "-c", '. "$1"; oms_with_file_lock "$2" python3 -c "$3" "$2" "$4" "$5"',
                      "fixture-expiry", str(panel.ROOT / "scripts/lib/file-lock.sh"),
                      str(project / ".oms/plan/tasks.json"), mutate, task_id, lease_state], env)
                plan_bytes = (project / ".oms/plan/tasks.json").read_bytes()
                shown = json.loads(call(plan_api + ["show", "--id", task_id], env))
                assert shown["claim_expired"] is False and shown["lease_id"] == marker["lease_id"], shown
                assert task_id not in call(plan_api + ["ready"], env).splitlines()
                next_claim = subprocess.run(plan_api + ["next", "--claim", "--provider", owner], env=env,
                                            capture_output=True, text=True, timeout=30)
                assert next_claim.returncode == 3 and "no actionable task" in next_claim.stderr, next_claim
                call(plan_api + ["reclaim", "--include-running", "--ttl", "1"], env)
                assert (project / ".oms/plan/tasks.json").read_bytes() == plan_bytes, "live exact lease changed"
            release.touch()
            # A write worker now captures a patch and an automatic review; full gates run this beside other suites.
            output, errors = child_process.communicate(timeout=120)
            assert child_process.returncode == 0, (output, errors)
            delegate_rows = [r for r in json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo",
                             str(project), "list", "1000", "--json"]))["rows"]
                             if r["kind"] == "delegate" and r.get("task_id") == task_id]
            assert delegate_rows and all(r["exit"] == 0 for r in delegate_rows), delegate_rows
            for indexed in delegate_rows:
                # The index uses Git's abbreviation; compare it to the actual full source object.
                short = indexed["base_sha"]
                assert len(short) >= 7 and source_sha.startswith(short), indexed
                assert subprocess.check_output(["git", "-C", str(source), "rev-parse", short], text=True).strip() == source_sha
                print("delegate source base:", task_id, short, source_sha)
            assert not list((project / ".oms/delegations").glob("*.json"))
            call(plan_api + ["reclaim", "--include-running", "--ttl", "1"], env)
            assert json.loads(call(plan_api + ["show", "--id", task_id], env))["state"] == "ready"
        finally:
            release.touch()
            if child_process.poll() is None:
                child_process.terminate()
            child_process.communicate(timeout=15)
    # A route that differs from the reviewed assignment, or an assignment panel cannot express, is refused before the claim.
    for refused_task, refused_assignment in (
            ("observed-mismatch", dict(provider="codex", model="gpt-6-luna", reasoning_effort="low")),
            ("observed-empty-model", dict(provider="codex", model="", reasoning_effort="high")),
            ("observed-fallback", dict(provider="codex", model="gpt-6-luna", reasoning_effort="high", fallback_model="gpt-6-sol"))):
        call(plan_api + ["init", "--goal", "Refused assignment fixture"])
        call(plan_api + ["add", "--id", refused_task, "--title", "Refused route", "--assignment", json.dumps(refused_assignment)])
        refused_env = dict(environment, OMS_ROOM_ID=test_rooms["claude"], OMS_ROOM_REPO=str(project),
                           OMS_ROOM_PARTICIPANT=mains["claude"][0], OMS_PANEL_MAIN_ATTEMPT=mains["claude"][1],
                           OMS_LOCK_DIR=str(temporary / "observed-locks"))
        refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_rooms["claude"],
                                  "--dispatch", "worker", "--owner", "claude", "--access", "write", "--purpose", "implement",
                                  "--to", "codex", "--model", "gpt-6-luna", "--reasoning-effort", "high",
                                  "--brief-file", str(brief), "--verify", ":", "--task-id", refused_task],
                                 env=refused_env, capture_output=True, text=True, timeout=60)
        named = "--to codex" + (" --model " + refused_assignment["model"] if refused_assignment["model"] else "")
        assert refused.returncode != 0 and named + " --reasoning-effort " + refused_assignment["reasoning_effort"] in refused.stderr, refused
        assert json.loads(call(plan_api + ["evidence-snapshot", "--id", refused_task], refused_env))["state"] == "ready"
    # One cross-repository read regression: dirty state A and a different scratch source B.
    untracked_state = project / "state-only-untracked.txt"
    untracked_state.write_text("owner state stays here\n")
    source_value = (source / "value.txt").read_text()
    (source / "value.txt").write_text(source_value + "pending scratch review\n")
    with (project / ".git/info/exclude").open("a") as exclude:
        exclude.write("\n__pycache__/\n.oms\n")
    state_status = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
    source_status = subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"])
    source_diff = subprocess.check_output(["git", "-C", str(source), "diff", "HEAD"])
    read_prompt = temporary / "scratch-read-prompt"
    read_verify = temporary / "scratch-read-verify"
    read_verify_env = temporary / "scratch-read-verify-env"
    test_room = test_rooms["claude"]
    main_member, main_attempt = mains["claude"]
    canonical_env = dict(environment, OMS_ROOM_ID=test_room, OMS_ROOM_REPO=str(project),
                         OMS_ROOM_PARTICIPANT=main_member, OMS_PANEL_MAIN_ATTEMPT=main_attempt,
                         OMS_LOCK_DIR=str(temporary / "observed-locks"))
    for role in ("advisor", "reviewer"):
        task_id = "scratch-" + role
        request = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
                   "--dispatch", role, "--owner", "claude",
                   "--prompt", "Inspect value.txt; judge only the supplied source evidence.", "--task-id", task_id]
        if role == "reviewer":
            request += ["--verify", "test -f value.txt"]
        preview = json.loads(call(request + ["--dry-run"], canonical_env))
        assert preview["argv"][preview["argv"].index("--repo") + 1] == str(source), preview
        if role == "reviewer":
            request[request.index("--verify") + 1] = ('mkdir -p __pycache__ verify-temp-dir; '
                'printf cache > __pycache__/x.pyc; printf output > verify-temp-dir/output; '
                'compgen -e > "$PANEL_TEST_VERIFY_ENV"; '
                'pwd -P > "$PANEL_TEST_VERIFY_LOG"; git rev-parse HEAD >> "$PANEL_TEST_VERIFY_LOG"')
        read_env = dict(canonical_env, PANEL_TEST_PROMPT=str(read_prompt),
                        PANEL_TEST_VERIFY_LOG=str(read_verify), PANEL_TEST_VERIFY_ENV=str(read_verify_env),
                        OMS_STATE_REPO=str(project),
                        OMS_CODEX_HARNESS_CAPABILITY="fixture-capability", OMS_HARNESS_BRIDGE="1",
                        ANTHROPIC_BASE_URL="http://fixture.invalid")
        if role == "reviewer":
            read_env.update([("OMS_OWNER_TOKEN", "fixture-owner"),
                             ("ANTHROPIC_AUTH_TOKEN", "fixture-bridge-token")])
        answer = call(request, read_env)
        assert ("artifact:" if role == "advisor" else "artifacts:") in answer, answer
        observed = json.loads(worker_log.read_text().splitlines()[-1])
        assert observed["cwd"] == str(source) and observed["parent"] == main_attempt and observed["room"] == test_room, observed
        assert observed["room_repo"] == observed["state_repo"] == str(project), observed
        if role == "reviewer":
            assert read_verify.read_text().splitlines() == [str(source), source_sha], "verify did not execute in B"
            verifier_env = set(read_verify_env.read_text().splitlines())
            assert not any(n.startswith(("OMS_ROOM_", "OMS_PANEL_")) for n in verifier_env), verifier_env
            assert not verifier_env.intersection({"OMS_STATE_REPO", "OMS_OWNER_TOKEN",
                "OMS_CODEX_HARNESS_CAPABILITY", "OMS_ATTEMPT_ID", "OMS_PLAN_LEASE_ID",
                "OMS_HARNESS_BRIDGE", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL"}), verifier_env
            verify_receipts = list((project / ".oms/artifacts/review").glob("_verify-*.md"))
            assert any(all(name in content for name in ("unset owner environment:", "OMS_STATE_REPO",
                "OMS_OWNER_TOKEN", "ANTHROPIC_AUTH_TOKEN"))
                for content in (p.read_text() for p in verify_receipts)), "verifier environment removal was not reported"
            assert "+pending scratch review" in read_prompt.read_text(), "review diff came from A"
            assert (source / "__pycache__/x.pyc").read_text() == "cache"
            assert (source / "verify-temp-dir/output").read_text() == "output"
            subprocess.run(["git", "-C", str(source), "check-ignore", "-q", "__pycache__/x.pyc"], check=True)
            shutil.rmtree(source / "verify-temp-dir")
        enrolled = room.participant(room.project(room.records(project, test_room)), observed["participant"])
        assert enrolled["role"] == role and enrolled["parent"] == main_member, enrolled
        report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                                 "--task-id", task_id, "--json"], canonical_env))
        assert report["rows"] and any(c.get("answer") for r in report["rows"] for c in r.get("calls", [])), report
        indexed = [r for r in json.loads(call(["bash", str(panel.ENTRY), "artifact-index", "--repo", str(project),
                    "list", "1000", "--json"]))["rows"] if r.get("task_id") == task_id]
        assert indexed and any(r.get("artifact") and (project / r["artifact"]).is_file() for r in indexed), indexed
        assert all(source_sha.startswith(r["base_sha"]) for r in indexed), indexed
        if role == "reviewer":
            scope_path = re.search(r"^review-scope: (.+)$", answer, re.M).group(1)
            scope = json.loads(Path(scope_path).read_text())
            assert scope["head"] == source_sha and scope["base"] == source_sha and scope["paths"] == ["value.txt"], scope
            import hashlib
            assert scope["diff_sha256"] == hashlib.sha256(source_diff).hexdigest(), scope
        attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
        attempt = next(a for a in attempts if a.get("task_id") == task_id)
        assert attempt["parent_attempt_id"] == main_attempt and attempt["refs"]["panel_location"].startswith("worktree:"), attempt
        assert not (source / ".oms").exists(), "read records split into B"
        assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == state_status
        assert subprocess.check_output(["git", "-C", str(source), "status", "--porcelain"]) == source_status
        assert (source / "value.txt").read_text() == source_value + "pending scratch review\n"
        assert untracked_state.read_text() == "owner state stays here\n"
        assert (project / "value.txt").read_text() == "old\n", "read pass edited A source"
        mutations = [(project / "value.txt", False), (project / "value.txt", True),
                     (source / "value.txt", False)]
        if role == "reviewer":
            mutations.append((source / "__pycache__/x.pyc", False))
        for target, strict in mutations:
            original = target.read_bytes()
            result = subprocess.run(request, env=dict(read_env, PANEL_TEST_MODE="primary-write",
                PANEL_TEST_PRIMARY=str(target), OMS_WORKER_GUARD_STRICT="1" if strict else "0"),
                capture_output=True, text=True, timeout=90)
            if target.parent == project and not strict:
                assert result.returncode == 0 and "warning: read pass observed state repository changes: tracked" in result.stderr, result
            else:
                assert result.returncode != 0 and "protected" in result.stderr, result
            assert target.read_bytes() != original, "read guard mutation fixture did not execute"
            target.write_bytes(original)
            shutil.rmtree(source / "verify-temp-dir", ignore_errors=True)
        if role == "reviewer":
            # A verifier runs before reviewers: its outputs may become the files
            # baseline, but its source/control writes must stop the review.
            def git_path(name):
                path = Path(call(["git", "-C", str(source), "rev-parse", "--git-path", name]).strip())
                return path if path.is_absolute() else source / path
            git_snapshots = {name: (git_path(name), git_path(name).read_bytes())
                             for name in ("HEAD", "index", "config")}
            hook = git_path("hooks") / "sec-13-hook"
            hook_verify = ('ln -s /dev/null "$(git rev-parse --git-path hooks)/sec-13-hook"'
                           if os.name != "nt" else
                           'printf hook > "$(git rev-parse --git-path hooks)/sec-13-hook"')
            protected_verifiers = [
                ("tracked", "printf verifier-edit >> value.txt", None),
                ("HEAD", 'printf "%s\\n" "$(git rev-parse HEAD)" > "$(git rev-parse --git-path HEAD)"; '
                    'printf "\\n" >> "$(git rev-parse --git-path HEAD)"', None),
                ("index", "git add value.txt", None),
                ("config", "git config --local sec13.fixture changed", None),
                ("hooks", hook_verify, None),
                (".oms", "mkdir -p .oms/plan; printf forged > .oms/plan/tasks.json", None),
                (".oms", "printf forged >> .oms/control.jsonl", "control.jsonl"),
                (".oms", "printf forged > .oms/plan/tasks.json", "plan/tasks.json"),
            ]
            for surface, verify, existing in protected_verifiers:
                shutil.rmtree(source / "verify-temp-dir", ignore_errors=True)
                if existing:
                    control = source / ".oms" / existing
                    control.parent.mkdir(parents=True, exist_ok=True)
                    (source / ".oms/.gitignore").write_text("*\n")
                    control.write_text("original\n")
                worker_before = worker_log.read_bytes()
                malicious_request = list(request)
                malicious_request[malicious_request.index("--verify") + 1] = verify
                result = subprocess.run(malicious_request, env=read_env, capture_output=True, text=True, timeout=90)
                assert result.returncode != 0 and "protected" in result.stderr and surface in result.stderr, result
                assert worker_log.read_bytes() == worker_before, "verifier mutation reached a reviewer"
                if surface == "tracked":
                    assert (source / "value.txt").read_text().endswith("verifier-edit")
                    (source / "value.txt").write_text(source_value + "pending scratch review\n")
                elif surface in git_snapshots:
                    target, original = git_snapshots[surface]
                    assert target.read_bytes() != original
                    target.write_bytes(original)
                elif surface == "hooks":
                    assert hook.is_symlink() if os.name != "nt" else hook.is_file()
                    hook.unlink()
                else:
                    assert (source / ".oms").exists(), "verifier control mutation did not execute"
                    shutil.rmtree(source / ".oms")
            if os.name != "nt":
                # An unchanged ignored link can hide control writes in the
                # verifier-output surface, so refuse it before verification.
                target_root = source / "__pycache__/control-target"
                target = target_root / "plan/tasks.json"
                target.parent.mkdir(parents=True)
                target.write_text("original control\n")
                (target_root / ".gitignore").write_text("*\n")
                verify_marker = temporary / "scratch-symlink-verifier-ran"
                for kind in ("root", "directory", "file"):
                    control_root = source / ".oms"
                    if kind == "root":
                        link, destination = control_root, target_root
                    else:
                        control_root.mkdir()
                        (control_root / ".gitignore").write_text("*\n")
                        if kind == "directory":
                            link, destination = control_root / "plan", target.parent
                        else:
                            (control_root / "plan").mkdir()
                            link, destination = control_root / "plan/tasks.json", target
                    link.symlink_to(destination, target_is_directory=kind != "file")
                    original_link = os.readlink(link)
                    worker_before = worker_log.read_bytes()
                    symlink_request = list(request)
                    symlink_request[symlink_request.index("--verify") + 1] = (
                        'printf forged > .oms/plan/tasks.json; printf ran > "$PANEL_TEST_SYMLINK_VERIFY_LOG"')
                    result = subprocess.run(symlink_request,
                        env=dict(read_env, PANEL_TEST_SYMLINK_VERIFY_LOG=str(verify_marker)),
                        capture_output=True, text=True, timeout=90)
                    assert result.returncode != 0 and "protected" in result.stderr and "symlink" in result.stderr, (kind, result)
                    assert ".oms" in result.stderr, (kind, result.stderr)
                    assert worker_log.read_bytes() == worker_before, "control symlink reached a reviewer"
                    assert not verify_marker.exists(), "control symlink reached the verifier"
                    assert os.readlink(link) == original_link and target.read_text() == "original control\n"
                    if kind == "root":
                        link.unlink()
                    else:
                        shutil.rmtree(control_root)
                shutil.rmtree(target_root)
            else:
                print("SKIP: protected control symlink fixtures require native symlink support")
            input_untracked = source / "review-input-untracked.txt"
            input_untracked.write_text("not verifier output\n")
            worker_before = worker_log.read_bytes()
            result = subprocess.run(request, env=read_env, capture_output=True, text=True, timeout=90)
            assert result.returncode != 0 and "cannot review untracked file content" in result.stderr, result
            assert worker_log.read_bytes() == worker_before, "untracked input reached a reviewer"
            input_untracked.unlink()
    shutil.rmtree(source / "__pycache__")
    # Cross-repository council pointers bind A; isolated seats still receive private copies.
    isolated_context = temporary / "scratch-read-isolated-context"
    isolated_context.mkdir()
    context = call(["bash", "-c", '''set -eu
    . "$1/scripts/lib/peer-common.sh"
    REPO="$2"; MA_READ_SOURCE_REPO="$3"; prompt_file="$4"
    ma_prepare_council_context
    ma_stage_council_context "$5"
    ma_export_child_env codex test "$REPO" "" read
    printf '%s\\n' "$MA_COUNCIL_CONTEXT_REL" "$OMS_STATE_REPO"
    ma_cleanup_council_context
    ''', "context", str(root), str(project), str(source), str(read_prompt), str(isolated_context)],
        dict(canonical_env, OMS_PANEL_DISPATCH="1"))
    pointer, state_root = context.strip().splitlines()
    assert pointer.startswith("$OMS_STATE_REPO/.oms/delegations/") and state_root == str(project), context
    relative_context = pointer.removeprefix("$OMS_STATE_REPO/")
    assert (isolated_context / relative_context / "request.md").is_file()
    assert (isolated_context / ".oms/artifacts" / Path(relative_context).name / "request.md").is_file()
    assert not (source / ".oms").exists()
    untracked_state.unlink()
    (source / "value.txt").write_text(source_value)
    # Snapshot failure is a frontend refusal, even before an observer binding can be selected.
    task_id = "frontend-owned"
    call(plan_api + ["init", "--goal", "Frontend snapshot refusal"])
    call(plan_api + ["add", "--id", task_id, "--title", "Owned snapshot"])
    call(plan_api + ["claim", "--id", task_id, "--provider", "claude"], canonical_env)
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(source), "--room", test_room,
               "--dispatch", "worker", "--owner", "claude", "--workload", "light", "--purpose", "investigate",
               "--prompt", "Inspect source without edits.", "--task-id", task_id]
    plan_path = project / ".oms/plan/tasks.json"
    original = plan_path.read_bytes()
    def frontend_refused(arguments, denied_env=canonical_env):
        started_before = worker_log.read_bytes()
        refused = subprocess.run(arguments, env=denied_env, capture_output=True, text=True, timeout=30)
        assert refused.returncode != 0 and refused.stderr.startswith("error:") and "plan protection is absent" not in refused.stderr, refused
        assert worker_log.read_bytes() == started_before, ("provider started after failed snapshot", refused)
    for hostile in (b"{", b"", b"[]", b'{"tasks":{},"tasks":{}}', b'{"tasks":[]}', b" " * (4 * 1024 * 1024 + 1)):
        plan_path.write_bytes(hostile)
        frontend_refused(request)
    plan_path.write_bytes(original)
    for linked in ("symbolic", "hard", "directory"):
        saved = plan_path.with_name("frontend-saved.json")
        plan_path.rename(saved)
        if linked == "symbolic":
            plan_path.symlink_to(saved)
        elif linked == "hard":
            os.link(saved, plan_path)
        else:
            plan_path.mkdir()
        try:
            frontend_refused(request)
        finally:
            plan_path.rmdir() if linked == "directory" else plan_path.unlink()
            saved.rename(plan_path)
    plan_path.chmod(0)
    try:
        frontend_refused(request)
    finally:
        plan_path.chmod(0o600)
    # Inject failures only at the frontend's authoritative snapshot boundary; the provider remains real/fake.
    harness = """import sys, subprocess
sys.path.insert(0, sys.argv[1])
import terminal_panel as p
real, mode = p.subprocess.run, sys.argv[2]
def query(args, **kwargs):
    if 'agent-plan' in args and 'evidence-snapshot' in args:
        if mode == 'timeout':
            raise subprocess.TimeoutExpired(args, 1)
        if mode == 'io':
            raise OSError('snapshot unreadable')
        if mode == 'missing':
            return subprocess.CompletedProcess(args, 2, '',
                'error: no such task: ' + args[args.index('--id') + 1] + '\\n'
                'hint: run `oms agent-plan --help` for usage\\n')
        return subprocess.CompletedProcess(args, 2 if mode == 'error' else 0,
            '' if mode == 'error' else ('{}' if mode == 'json' else '{'),
            'error: current-owner snapshot failed' if mode == 'error' else '')
    return real(args, **kwargs)
p.subprocess.run = query
sys.exit(p.main(sys.argv[3:]))
"""
    for mode in ("error", "missing", "json", "malformed", "timeout", "io"):
        frontend_refused(["python3", "-c", harness, str(panel.ROOT / "scripts/lib"), mode, *request[3:]])
    # Proven task absence and no-plan compatibility still execute, explicitly without protection.
    for no_plan in (False, True):
        saved = plan_path.with_name("frontend-no-plan.json")
        if no_plan:
            plan_path.rename(saved)
        try:
            absent = subprocess.run(request[:-1] + ["frontend-absent"], env=canonical_env,
                                    capture_output=True, text=True, timeout=30)
            assert absent.returncode == 0 and "plan protection is absent" in absent.stderr, absent
        finally:
            if no_plan:
                saved.rename(plan_path)
    assert plan_path.read_bytes() == original, "frontend compatibility changed the owned row"
    call(["bash", str(panel.ENTRY), "scratch-worktree", "remove", "--repo", str(project), str(source)])
    print("observed worker leases: real markers, source-index lineage, scratch non-worker results, frontend refusal and recovery passed")
