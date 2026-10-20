execution_repo = temporary / "verification-execution-root"
execution_repo.mkdir()
import panel_dispatch as dispatch
from contextlib import redirect_stderr
from unittest.mock import patch
import io
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
try:
    panel.run_dispatch(root, "codex", "worker", "read", "inspect", None,
                       "bash tests/scripts-smoke.sh --only test_oms_allowlist_stays_in_sync_with_scripts",
                       None, True, None, {"kind": "worker", "purpose": "investigate"}, None,
                       None, None, work_repo=execution_repo)
    raise AssertionError("verifier present only in source repo was accepted")
except ValueError as error:
    assert "missing suite file" in str(error), error
try:
    panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section not-a-section")
    raise AssertionError("unknown operator section was accepted")
except ValueError as error:
    assert "valid names" in str(error), error
panel._validate_dispatch_verifier(root, "bash tests/operator-tools-smoke.sh --section dispatch-exit,dispatch-results,core/dashboard")
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
    answer = call(command, write_env)
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
         ("claude", "builder", None, "write", None, "gpt-6-luna"),
         ("claude", "advisor", None, "read", None, "gpt-6-astra"),
         ("codex", "advisor", "deep", "read", None, "claude-opus-5-5"),
         ("claude", "reviewer", None, "read", None, "gpt-6-astra"),
         ("claude", "security-auditor", None, "read", None, "gpt-6-astra"),
         ("codex", "researcher", None, "read", None, "claude-haiku-5-5"),
         ("claude", "researcher", None, "read", None, "gpt-6-luna")]
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
    if role == "advisor":
        retained = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                                    "--task-id", "panel-route-%s" % index, "--json"]))["rows"][0]
        assert any(c.get("kind") == "call" and "inspected source" in c.get("answer", "") for c in retained["calls"]), retained
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

# A researcher keeps the cheap family, read-only strategy and source evidence.
from panel_routing import newest_route_model
assert newest_route_model("codex", "gpt-6-luna", ["gpt-6.2-luna", "gpt-6.10-luna", "gpt-7-astra"]) == "gpt-6.10-luna"
assert newest_route_model("claude", "claude-haiku-5-5", ["claude-haiku-5-6", "claude-opus-9-9"]) == "claude-haiku-5-6"
codex_only = {"provider": "codex", "binary": "codex", "installed": True, "exact_model_override": True, "access": ["read", "write"], "models": ["gpt-6-sol"],
              "catalog_models": ["gpt-6-sol", "gpt-6.1-sol", "gpt-6.2-sol", "gpt-7-astra"]}
with patch.object(panel, "provider_catalog", return_value=[codex_only]):
    assert panel.selected_route("codex", "worker", "deep", "read", "investigate")["model"] == "gpt-6.2-sol"
    assert panel.selected_route("codex", "worker", "light", "read", "explain")["model"] == "gpt-6-luna"
    assert panel.selected_route("codex", "reviewer", None, "read", None)["provider"] == "codex"
    explicit = panel.selected_route("claude", "worker", None, "read", None, "codex")
    assert explicit["provider"] == "codex" and explicit["model"] == "gpt-6-luna" and explicit["effort"] == "medium"
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
assert alternate_route(automatic, provider_status={other: {"weekly_used": 89}})["provider"] == other
assert alternate_route(automatic, provider_status={other: {"weekly_used": None}})["provider"] == other
assert alternate_route(automatic, provider_status={}) ["provider"] == other
assert alternate_route(automatic, provider_status={other: {"weekly_used": 90}}) is None
assert alternate_route(automatic, {"codex", "claude"}, {other: {"weekly_used": 97.2}}) is None
with patch.object(panel, "selected_route", return_value=explicit), \
        patch.object(panel, "provider_catalog", side_effect=AssertionError("explicit --to queried fallback")), \
        patch.object(panel, "run_dispatch", side_effect=lambda *args: args[-2]):
    assert panel.dispatch(project, "claude", "worker", None, "read", None,
                          prompt="explicit route", target="codex", model="gpt-6-luna") is None
recorded = {"codex": {"weekly_used": 90}, "claude": {"weekly_used": None}}
claude_candidate = {"provider": "claude", "binary": "claude", "installed": True, "models": [], "exact_model_override": True, "access": ["read", "write"]}
def dispatched_fallback(readings):
    catalog = [dict(row, installed=True) for row in (codex_only, claude_candidate)]
    with patch.object(panel, "selected_route", return_value=dict(automatic, provider="claude", model="claude-sonnet-5-5",
                                                                  binary="claude", effort="medium")), \
            patch.object(panel, "provider_catalog", side_effect=lambda name="all": [row for row in catalog if row["provider"] == name]), \
            patch.object(panel, "routing_status", return_value=readings), \
            patch.object(panel, "run_dispatch", side_effect=lambda *args: args[-2]):
        return panel.dispatch(project, "claude", "worker", None, "write", "implement", brief=str(fallback_brief),
                              verify="true")
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
    for role in ("reviewer", "verifier", "security-auditor", "critic", "advisor", "release-engineer"):
        judge = panel.selected_route("claude", role, None, "read", None)
        try:
            panel.routed_command(panel.ENTRY, project, judge, brief=str(brief), verify="true", resume="fixture-session")
            raise AssertionError("%s resumed a session" % role)
        except ValueError as error:
            assert "only write-capable roles continue" in str(error), error
    # Among read roles only a reviewer runs a verifier; the rest refuse an explicit one and name the reviewer.
    for role in ("verifier", "security-auditor", "critic", "worker", "advisor"):
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

# Registered transports join the same task tree without a native launcher or
# changes to preset allocation. Muse is a synthetic custom-adapter fixture.
external_base = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                 "--owner", "codex", "--purpose", "investigate", "--prompt", "Inspect value.txt without edits."]
for target, expected in (("grok", "grok"), ("agy", "antigravity"), ("muse", "muse")):
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
    if target == "muse":
        assert execution["argv"][0] == "run" and "--access" in execution["argv"]
        assert execution["argv"][execution["argv"].index("--model") + 1] == "muse-fixture"

for options in (["--to", "muse"], ["--to", "muse", "--model", "provider-default"],
                ["--to", "muse,grok", "--model", "fixture"], ["--to", "unregistered", "--model", "fixture"],
                ["--to", "deepseek", "--model", "fixture"], ["--to", "codex", "--model", "gpt-5.6-sol"],
                ["--to", "grok", "--model", "fixture", "--workload", "deep"],
                ["--model", "fixture"], ["--reasoning-effort", "extreme"],
                ["--to", "muse", "--model", "fixture", "--access", "write", "--purpose", "implement",
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
for role, target in (("advisor", "grok"), ("reviewer", "muse")):
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
opted_in = dict(environment, OMS_PROVIDER_WRITE_ADAPTERS="muse")
write_request = [a for a in external_base[:external_base.index("--purpose")]] + [
    "--to", "muse", "--model", "muse-fixture", "--access", "write", "--purpose", "implement",
    "--brief-file", str(brief), "--verify", 'test "$(tail -1 value.txt)" = new', "--task-id", "external-muse-write",
    "--label", "Implement with adapter"]
assert json.loads(call(write_request + ["--dry-run"], opted_in))["route"]["access"] == "write"
write_env = dict(opted_in, PANEL_TEST_MODE="write", OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"])
assert "patch:" in call(write_request, write_env)
assert (project / "value.txt").read_text() == "old\n"
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show", side_effect=empty_control), \
        patch.object(panel, "provider_catalog", return_value=list(registered.values())), \
        patch.object(panel, "dispatch", return_value=0) as routed, \
        patch.object(panel.sys, "stdout", io.StringIO()), \
        patch.object(panel, "read_field", side_effect=["p", "muse", "5", "muse-fixture", "", "Inspect source", "", "q"]):
    assert panel.interactive(project) == 0
    assert routed.call_args.kwargs == {"target": "muse", "model": "muse-fixture", "effort": None}
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

# The council reuses the peer front door: four seats over two families, explicit rounds,
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
        "claude:model=claude-opus-5-5,codex:model=gpt-6.1-sol,claude:model=claude-sonnet-5-5,codex:model=gpt-6-luna"
    assert len(planned["argv"][planned["argv"].index("--providers") + 1].split(",")) == 4 == planned["seats"]
frontier_plan = json.loads(call(council_args + ["--workload", "frontier", "--dry-run"]))
assert frontier_plan["argv"][frontier_plan["argv"].index("--providers") + 1] == \
    "claude:model=claude-opus-5-5,codex:model=gpt-6.1-sol,claude:model=claude-fable-5-1,codex:model=gpt-6-astra"
assert (project / ".oms/artifacts/index.jsonl").read_bytes() == before_index
# A council rejects a tier that no longer preserves two-family independence.
one_route = deepcopy(panel.policy())
one_route["tiers"]["deep"] = one_route["tiers"]["deep"][:1]
one_route["tiers"]["routine"] = one_route["tiers"]["routine"][:1]
with patch.object(panel, "policy", return_value=one_route):
    try:
        panel.council(project, "codex", "council-one-route", "One route", "Compare options.", dry_run=True)
    except ValueError as error:
        assert "two independent provider families" in str(error)
    else:
        raise AssertionError("council started with one provider family")
call(council_args, dict(environment, OMS_PANEL_MAIN_ATTEMPT=main_attempts["codex"]))
council_attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
council_attempts = [a for a in council_attempts if a.get("task_id") == "panel-council-test"]
assert any(a["tool"] == "panel-council" and a["state"] == "done" for a in council_attempts)
assert {a["refs"].get("panel_model") for a in council_attempts if a["tool"] != "panel-council"} == \
    {"gpt-6.1-sol", "claude-opus-5-5", "claude-sonnet-5-5", "gpt-6-luna"}, council_attempts
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
from panel_view import render_results
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
with patch.object(review_settlement, "_stopped", return_value=True), \
        patch.object(review_settlement, "_apply") as settle_apply:
    report = review_settlement._settle(project, [old_worker, new_worker], [],
        {main_attempts["codex"]}, {"elsewhere-task": (decision, result_row)}, apply=True)
assert report["applied"] == [old_worker["attempt_id"]], report
assert report["pending"][0]["attempt"] == new_worker["attempt_id"]
assert report["pending"][0]["pending_reason"] == "newer_than_decision", report
assert settle_apply.call_count == 1
# Exercise persistence and lifecycle writes: settlement must not mint another
# result revision or notification when an identical finalize is retried.
event_command = ["bash", str(panel.ENTRY), "agent-events", "--repo", str(project)]
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
    deferred = saved.finalize(project, "codex", dead_task, str(summary), "completed")
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
unproven_worker = settlement_worker("settlement-no-exit-task", legacy=True, exit_proof=False)
standalone_running = settlement_worker("settlement-running-task", review=False, legacy=True)
standalone_args = recover[:recover.index("--owner-attempt")] + ["--standalone", "--older-than", "1", "--json"]
young = json.loads(call(standalone_args))
assert standalone_worker in {r["attempt"] for r in young["pending"] if r["pending_reason"] == "review_too_recent"}, young
with patch.object(review_settlement, "_age", return_value=None), \
        patch.dict(os.environ, environment, clear=True):
    unknown_age = review_settlement.settle_review(project, standalone=True, older_than=1)
assert standalone_worker in {r["attempt"] for r in unknown_age["pending"] if r["pending_reason"] == "review_age_unknown"}
time.sleep(1.1)
standalone_args[standalone_args.index("--older-than") + 1] = "0.00000001"
preview = json.loads(call(standalone_args))
assert standalone_worker in {r["attempt"] for r in preview["candidates"]}, preview
assert not preview["executes"] and not preview["applied"], preview
assert unproven_worker in {r["attempt"] for r in preview["pending"] if r["pending_reason"] == "exit_unknown"}, preview
assert not {standalone_running, running} & {r["attempt"] for r in preview["candidates"]}, preview
standalone_rows, standalone_artifacts = saved._records(project, full=True)
standalone_attempt = next(a for a in standalone_rows if a["attempt_id"] == standalone_worker)
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
standalone_final = json.loads(call(event_command + ["show", "--attempt", standalone_worker, "--json"]))
assert standalone_final["state"] == "cancelled" and standalone_final["reason_code"] == "owner_superseded", standalone_final
assert standalone_final["refs"]["owner_superseded_reason"] == "aged standalone review", standalone_final
assert standalone_worker not in json.loads(call(standalone_args + ["--apply"]))["applied"]
# The explicit seven-day policy covers undecided legacy work and missing exits.
# Append through the lifecycle API with a controlled event clock, never rewrite
# the fixture ledger; a later review entry must replace the earlier review age.
lifecycle = __import__("runpy").run_path(str(root / "scripts/lib/agent-events.py"))
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
assert json.loads(call(write_finalize))["outcome"] == "accepted"
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
# BEGIN patch-scope finalize regression (also runnable as an isolated case).
scope_patch = project / ".oms/artifacts/scope-result.patch"
scope_worker = {"attempt_id": "att_scope_write", "task_id": "scope-result", "parent_attempt_id": "scope-main",
                "refs": {"panel_role": "worker", "panel_access": "write", "panel_room_id": "scope-room",
                         "panel_room_participant": "scope-worker"}}

def scope_evidence(body):
    scope_patch.write_bytes(body)
    digest = hashlib.sha256(body).hexdigest()
    return [{"kind": "delegate", "task_id": "scope-result", "attempt_id": "att_scope_write",
             "patch": ".oms/artifacts/scope-result.patch", "patch_sha256": digest, "exit": 0},
            {"kind": "patch-land", "task_id": "scope-result", "patch_sha256": digest, "exit": 0}]

extra_patch = (b"diff --git a/value.txt b/value.txt\n--- a/value.txt\n+++ b/value.txt\n@@ -1 +1 @@\n-old\n+new\n"
               b"diff --git a/other.txt b/other.txt\n--- /dev/null\n+++ b/other.txt\n@@ -0,0 +1 @@\n+extra\n")
scope_member = {"participants": [{"participant": "scope-worker", "owns": ["value.txt"]}]}
with patch.dict(os.environ, environment, clear=True), \
        patch.object(saved, "_plan_bindings", return_value={}), \
        patch.object(saved, "_register"), patch.object(saved, "_plan_hint"), \
        patch.object(saved.room, "status", return_value=scope_member):
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
retry_worker = dict(scope_worker, created_at="2026-10-16T00:00:02Z", state="review")
failed_worker = dict(retry_worker, attempt_id="att_retry_failed", state="failed",
                     created_at="2026-10-16T00:00:01Z")
failed_row = {"kind": "delegate", "task_id": "scope-result", "attempt_id": failed_worker["attempt_id"], "exit": 7}
with patch.dict(os.environ, environment, clear=True), patch.object(saved, "_register"), \
        patch.object(saved, "_plan_hint"), patch.object(saved, "_worker_owns", return_value=["value.txt"]):
    for state in ("failed", "timed_out", "cancelled", "abandoned"):
        previous = dict(failed_worker, state=state)
        with patch.object(saved, "_records", return_value=([previous, retry_worker], [failed_row] + retry_rows)):
            accepted_retry = saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
            assert accepted_retry["admission_event_ids"] == ["evt_retry_landed"], accepted_retry
            assert accepted_retry["superseded_attempts"] == [{"attempt_id": previous["attempt_id"],
                "superseded_by": retry_worker["attempt_id"], "reason": state}], accepted_retry
            with patch.object(saved, "_load_result", return_value=accepted_retry):
                report_rows = [failed_row] + retry_rows + [{"kind": "panel-result", "task_id": "scope-result"}]
                with patch.object(saved, "_records", return_value=([previous, retry_worker], report_rows)):
                    view = saved.results(project, "scope-result")["rows"][0]
                    assert view["representative_state"] == "accepted" and view["calls"][0]["superseded"], view
            settled_retry = review_settlement._settle(project, [previous], [], {"scope-main"},
                {"scope-result": (accepted_retry, {"ts": "2026-10-17T00:00:00Z"})})
            assert not settled_retry["pending"] and settled_retry["superseded"][0]["attempt"] == previous["attempt_id"]
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
            (dict(failed_worker, refs=dict(failed_worker["refs"], panel_alias="W2")), retry_worker, retry_rows),
            (failed_worker, retry_worker, retry_rows[:1])):
        with patch.object(saved, "_records", return_value=([previous, candidate], [failed_row] + rows)):
            try:
                saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
                raise AssertionError("unaccounted attempt escaped admission")
            except ValueError as error:
                assert "matching landed patch" in str(error), error
# END patch-scope finalize regression.

whole = json.loads(call(whole_finalize, owner_env))
assert whole["main_attempt_id"] == main_attempts["claude"] and not whole["admission_event_ids"]
joined = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                         "--task-id", "whole-main-result", "--json"]))["rows"][0]
assert any(c.get("kind") == "delegate" for c in joined["calls"]), joined
assert joined["title"] == whole["summary"].split("\n")[0]
assert all(joined[key] is None for key in ("model", "role", "location", "process_state"))
parent_task = {"task_id": "parent-read", "main_attempt_id": "main-parent", "revision": "a" * 24,
               "outcome": "completed", "verification": {"status": "passed"},
               "summary": "Owner summary for the parent read.\nMore detail."}
unrelated_child = {"task_id": "failed-child", "attempt_id": "att_failed_child", "parent_attempt_id": "main-parent",
                   "state": "failed", "refs": {"panel_label": "Unrelated failed write", "panel_model": "gpt-test",
                   "panel_role": "worker", "panel_location": "remote"}}
failed_call = {"kind": "delegate", "task_id": "failed-child", "attempt_id": "att_failed_child", "exit": 1}
parent_result_row = {"kind": "panel-result", "task_id": "parent-read"}
with patch.object(saved, "_records", return_value=([unrelated_child], [parent_result_row, failed_call])), \
        patch.object(saved, "_load_result", return_value=parent_task):
    parent = saved.results(project, "parent-read")["rows"][0]
    assert parent["title"] == "Owner summary for the parent read."
    assert parent["issue"] is None and parent["outcome"] == "completed"
    assert parent["verification"]["status"] == "passed"
    assert parent["model"] is None and parent["role"] is None and parent["location"] is None
    assert parent["process_state"] is None and parent["calls"] == [{"kind": "delegate", "exit": 1,
                                                                    "attempt_id": "att_failed_child"}]
own_task_result = dict(parent_task, task_id="own-task", summary="Own task summary.")
own_attempt = {"task_id": "own-task", "attempt_id": "att_own_task", "parent_attempt_id": "main-parent",
               "state": "done", "refs": {"panel_label": "Own task title", "panel_model": "own-model",
               "panel_role": "reviewer", "panel_location": "local"}}
own_call = {"kind": "call", "task_id": "own-task", "attempt_id": "att_own_task", "exit": 0}
with patch.object(saved, "_records", return_value=([own_attempt, unrelated_child],
        [{"kind": "panel-result", "task_id": "own-task"}, own_call, failed_call])), \
        patch.object(saved, "_load_result", return_value=own_task_result):
    own_metadata = saved.results(project, "own-task")["rows"][0]
    assert (own_metadata["title"], own_metadata["model"], own_metadata["role"],
            own_metadata["location"], own_metadata["process_state"]) == (
                "Own task title", "own-model", "reviewer", "local", "done")
    assert [call["attempt_id"] for call in own_metadata["calls"]] == ["att_own_task"]
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
    endings = [
        ("cancelled", {"exit": 130, "state": "cancelled", "reason_code": "cancelled_by_owner"},
         "implement cancelled", "cancelled", "none"),
        ("guard", {"exit": 1, "state": "failed", "guard": "scope"},
         "implement guard stop (scope)", "guard stop (scope)", "inspect guard stop and salvage partial work"),
        ("timeout", {"exit": 124, "state": "timed_out"},
         "timed out", "timed out", "continue with --continue TASK_ID; at the revision-round cap use the owner override"),
        ("verify", {"exit": 0, "verify_exit": 1, "state": "done", "reason_code": "verification_failed"},
         "verify failed", "verify failed", "retry verify"),
        ("provider", {"exit": 1, "state": "failed", "provider_error_exit": 1},
         "implement provider error", "provider error", "retry implement"),
        ("failure", {"exit": 1, "state": "failed"}, "implement failed", "failed", "retry implement"),
    ]
    for suffix, evidence, expected_state, expected_stage_state, expected_action in endings:
        item = dict(task_id="ending-" + suffix, kind="delegate", attempt_id="att-ending-" + suffix, **evidence)
        with patch.object(saved, "_records", return_value=([], [item])):
            projected = saved.results(project, task_id="ending-" + suffix)["rows"][0]
        assert projected["representative_state"] == expected_state, projected
        assert projected["next_action"] == expected_action, projected
        stage_name = "verify" if suffix == "verify" else "implement"
        assert projected["stages"][stage_name]["state"] == expected_stage_state, projected
        rendered = question_view.render_results({"rows": [projected]}, width=200)
        assert expected_state in rendered and expected_action in rendered, rendered
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
    for text in ("/private/absolute", "".join(("api", "_key", "=", "fixture-value"))):
        summary.write_text(text)
        try:
            saved.finalize(project, "codex", "bad-result", str(summary), "completed")
            raise AssertionError("unsafe summary accepted")
        except ValueError:
            pass
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
            assert "panel evidence must be a regular non-symlink, non-hard-linked file" in error_text.getvalue(), error_text.getvalue()
    except (OSError, NotImplementedError):
        if os.name != "nt":
            raise
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
