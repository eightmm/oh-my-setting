# Ordered integration dependency: provider routing -> native controls -> room protocol
# -> cross-repository dispatch -> continuation/lineage -> scope/admission.
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys

_operator_block("provider-and-routing")
baseline = subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"])
report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--json"]))
assert report["kind"] == "oms-panel" and report["schema"] == 1
assert all(report["providers"][p]["installed"] for p in panel.PROVIDERS)
assert report["dashboard"]["kind"] == "oms-dashboard"
assert not (project / ".oms").exists(), "panel query created a state store"
assert subprocess.check_output(["git", "-C", str(project), "status", "--porcelain"]) == baseline

# Discovery is registry-driven and does not execute even marked adapters.
inventory = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--providers", "--json"]))
registered = {row["provider"]: row for row in inventory["providers"]}
assert inventory["executes"] is False and inventory["kind"] == "oms-panel-providers"
assert not log.exists() and not worker_log.exists() and not (project / ".oms").exists()
for name in ("grok", "antigravity", "muse"):
    assert registered[name]["installed"] and not registered[name]["native_launch"], registered[name]
    assert registered[name]["session_messaging"] == "room_polling"
    assert registered[name]["native_hook_wiring"] == "unverified"
assert registered["muse"]["access"] == ["read"]
assert registered["codex"]["native_launch"] and registered["claude"]["native_launch"]
assert shlex.split(panel.shell_command(["bash", str(panel.ENTRY), str(project)]))[1:] == ["bash", str(panel.ENTRY), str(project)]
# Roles pick by tier, quota and availability; missing readings spread load away from the owner.
both = {"claude", "codex"}
assert allocate("codex", tier="light", installed=both)["model"] == "claude-haiku-5-5"
assert allocate("claude", tier="light", installed={"codex"})["model"] == "gpt-6-luna"
assert allocate("codex", installed={"claude"})["model"] == "claude-sonnet-5-5"
assert allocate("claude", installed={"codex"})["model"] == "gpt-6-luna"
assert not any("prefer" in rule for rule in policy()["roles"].values()), "default roles carry no provider preference"
for role, model in (("worker", "claude-sonnet-5-5"), ("builder", "claude-sonnet-5-5"), ("refactorer", "claude-sonnet-5-5"),
                    ("release-engineer", "claude-sonnet-5-5"),
                    ("performance-engineer", "claude-opus-5-5"), ("integrator", "claude-opus-5-5")):
    for owner in both:
        expected = model if owner == "codex" else ("gpt-6.1-sol" if role in (
            "performance-engineer", "integrator") else "gpt-6-luna")
        assert allocate(owner, role=role, installed=both)["model"] == expected
    assert allocate("claude", role=role, installed={"codex"})["provider"] == "codex"
assert allocate("codex", role="documenter", installed=both)["model"] == "claude-haiku-5-5"
assert allocate("claude", role="documenter", installed={"codex"})["model"] == "gpt-6-luna"
assert allocate("claude", role="advisor", installed=both)["model"] == "gpt-6-astra"
assert allocate("claude", role="advisor", installed={"codex"})["model"] == "gpt-6-astra"
assert allocate("claude", role="verifier", installed={"claude"})["model"] == "claude-fable-5-1"
assert allocate("claude", role="architect", installed=both)["model"] == "gpt-6-astra"
for role in ("reviewer", "verifier", "security-auditor", "critic"):
    assert allocate("claude", role=role, installed=both)["provider"] == "codex"
    assert allocate("codex", role=role, installed=both)["provider"] == "claude"
assert allocate("codex", role="researcher", installed=both)["model"] == "claude-haiku-5-5"
for used, expected in ((89, "claude"), (90, "codex"), (100, "codex"),
                       (None, "claude"), (True, "claude"), ("90", "claude"), (101, "claude")):
    readings = {"claude": {"weekly_used": used}}
    assert allocate("codex", installed=both, provider_status=readings)["provider"] == expected
assert allocate("claude", role="reviewer", installed=both,
                provider_status={"codex": {"weekly_used": 90}})["provider"] == "claude"
# With both readings, more weekly quota left wins; missing readings put the owner last.
for claude_used, codex_used, expected in ((60, 20, "codex"), (20, 60, "claude")):
    readings = {"claude": {"weekly_used": claude_used}, "codex": {"weekly_used": codex_used}}
    assert allocate("claude", role="builder", installed=both, provider_status=readings)["provider"] == expected, readings
    assert allocate("codex", role="integrator", installed=both, provider_status=readings)["provider"] == expected, readings
# Readings inside the tie band (both under 50%, or within 15 points) are equal: fewer recent dispatches
# win, else the provider opposite the main.
for claude_used, codex_used in ((2, 11), (40, 40), (11, 2), (70, 60), (49, 5)):
    readings = {"claude": {"weekly_used": claude_used}, "codex": {"weekly_used": codex_used}}
    for owner in both:
        other = "codex" if owner == "claude" else "claude"
        assert allocate(owner, role="builder", installed=both, provider_status=readings)["provider"] == other, readings
        assert allocate(owner, role="builder", installed=both, provider_status=readings,
                        recent={other: 3, owner: 1})["provider"] == owner, readings
        assert allocate(owner, role="builder", installed=both, provider_status=readings,
                        recent={other: 2, owner: 2})["provider"] == other, readings
        assert allocate(owner, role="builder", installed=both, provider_status=readings,
                        recent={owner: 1})["provider"] == other, readings
# Outside the band the quota decides whatever the recent counts say.
outside = {"claude": {"weekly_used": 20}, "codex": {"weekly_used": 60}}
assert allocate("claude", role="builder", installed=both, provider_status=outside,
                recent={"claude": 9})["provider"] == "claude"
assert policy()["tie_band"] == {"under": 50, "gap": 15}
# Independent judges avoid the reviewed task's writer; without a known writer the main's opposite decides.
for role in ("reviewer", "verifier", "security-auditor", "critic"):
    for owner in both:
        for writer in both:
            judge = allocate(owner, role=role, installed=both, writer=writer)["provider"]
            assert judge != writer, (role, owner, writer)
        assert allocate(owner, role=role, installed=both, writer="unknown")["provider"] != owner
assert allocate("claude", role="reviewer", installed={"codex"}, writer="codex")["provider"] == "codex"
for readings in ({}, {"claude": {"weekly_used": 60}}, {"codex": {"weekly_used": 20}}):
    for owner in both:
        assert allocate(owner, role="builder", installed=both, provider_status=readings)["provider"] != owner
assert allocate("claude", role="reviewer", installed=both,
                provider_status={"claude": {"weekly_used": 80}, "codex": {"weekly_used": 10}})["provider"] == "codex"
assert allocate("codex", role="reviewer", installed=both,
                provider_status={"claude": {"weekly_used": 80}, "codex": {"weekly_used": 10}})["provider"] == "claude"
assert allocate("codex", installed={"codex"}, provider_status={"codex": {"weekly_used": 100}})["provider"] == "codex"
# Routing spread follows quota readings except that independent judgments always avoid the owner.
routing_roles = {
    "researcher": "light",
    "worker": "routine", "builder": "routine", "documenter": "light",
    "integrator": "deep", "performance-engineer": "deep",
    "reviewer": "frontier", "verifier": "frontier", "security-auditor": "frontier",
    "critic": "frontier",
    "advisor": "frontier", "architect": "frontier",
}
usage_scenarios = {
    "none": {},
    "claude-70-codex-20": {"claude": {"weekly_used": 70}, "codex": {"weekly_used": 20}},
    "codex-70-claude-20": {"claude": {"weekly_used": 20}, "codex": {"weekly_used": 70}},
}
for role, tier in routing_roles.items():
    for scenario, readings in usage_scenarios.items():
        for owner in both:
            selected = allocate(owner, role=role, installed=both, provider_status=readings)
            expected = {"none": "codex" if owner == "claude" else "claude", "claude-70-codex-20": "codex",
                        "codex-70-claude-20": "claude"}[scenario]
            if role in ("reviewer", "verifier", "security-auditor", "critic"):
                expected = "codex" if owner == "claude" else "claude"
            assert selected["provider"] == expected, (role, scenario, owner, selected)
            assert selected["workload"] == tier
            assert selected["model"] == {
                ("light", "claude"): "claude-haiku-5-5", ("light", "codex"): "gpt-6-luna",
                ("routine", "claude"): "claude-sonnet-5-5", ("routine", "codex"): "gpt-6-luna",
                ("deep", "claude"): "claude-opus-5-5", ("deep", "codex"): "gpt-6.1-sol",
                ("frontier", "claude"): "claude-fable-5-1", ("frontier", "codex"): "gpt-6-astra",
            }[(tier, expected)]
try:
    allocate("codex", installed=both, provider_status={p: {"weekly_used": 90} for p in both})
    raise AssertionError("all saturated candidates were accepted")
except ValueError as error:
    assert "<=10% weekly quota left" in str(error), error
routes = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--routes"]))
assert set(routes["tiers"]) == set(panel.TIERS) and set(routes["roles"]) == set(routes["kinds"]) == set(panel.KINDS)
assert routes["writers"] == sorted(panel.WRITERS) and report["routing"]["roles"] == routes["roles"]
assert set(routes["chosen"]) == set(panel.KINDS)
from panel_view import PURPOSE_NAMES
assert set(routes["roles"]) <= set(PURPOSE_NAMES), "every role strategy needs a board label"
for role, rule in routes["roles"].items():
    expected = allocate(routes["owner"], role, installed=both)
    assert routes["chosen"][role] == {k: expected[k] for k in ("provider", "model", "effort")}
    assert rule["tier"] == expected["workload"]
from panel_routing import policy
configured = policy()
assert configured["review_backlog_warn"] == 8
assert configured["auto_review"] is True
for invalid_threshold in (True, -1, "8"):
    with patch("panel_routing.Path.read_text", return_value=json.dumps(dict(configured, review_backlog_warn=invalid_threshold))):
        try:
            policy()
        except ValueError:
            pass
        else:
            raise AssertionError("invalid review backlog threshold accepted")
for invalid in ("routine", {"tier": "missing"}, {"tier": "routine", "prefer": "grok"},
                {"tier": []}, {"tier": "routine", "prefer": None}, {"tier": "routine", "extra": True}):
    malformed = deepcopy(policy())
    malformed["roles"]["builder"] = invalid
    with patch("panel_routing.Path.read_text", return_value=json.dumps(malformed)):
        try:
            policy()
            raise AssertionError("invalid role policy accepted: %r" % invalid)
        except ValueError:
            pass
for markers, expected in (({}, "codex"), ({"CODEX_SANDBOX": "1"}, "codex"),
                          ({"CLAUDECODE": "1", "CODEX_SANDBOX": "1"}, "claude"),
                          ({"CLAUDE_CODE_ENTRYPOINT": "cli"}, "claude"),
                          ({"OMS_AGENT": "codex", "CLAUDECODE": "1"}, "codex"),
                          ({"OMS_AGENT": "claude", "CODEX_SANDBOX": "1"}, "claude")):
    with patch.dict(os.environ, markers, clear=True):
        assert panel.caller_owner() == expected
    detected_env = {k: v for k, v in environment.items()
                    if k not in {"OMS_AGENT", "CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "CODEX_SANDBOX"}}
    detected_env.update(markers)
    planned = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "reviewer",
                              "--prompt", "Inspect source.", "--verify", "true", "--dry-run"], detected_env))
    assert planned["route"]["owner"] == expected
    assert planned["route"]["provider"] != expected
    with patch.dict(os.environ, detected_env, clear=True), patch.object(panel, "council", return_value=0) as detected:
        assert panel.main(["--repo", str(project), "--council", "--task-id", "detected-council",
                           "--label", "Inspect source", "--prompt", "Inspect source.", "--dry-run"]) == 0
        assert detected.call_args.args[1] == expected
for target, model, effort in (("codex", "gpt-6-luna", "medium"), ("claude", "claude-sonnet-5-5", "medium")):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
               "--to", target, "--prompt", "Inspect source.", "--dry-run"]
    planned = json.loads(call(request))
    assert (planned["route"]["provider"], planned["route"]["model"], planned["route"]["effort"]) == (target, model, effort)
    assert planned["argv"][planned["argv"].index("--model") + 1] == model
    overridden = json.loads(call(request + ["--model", model, "--reasoning-effort", "high"]))
    assert overridden["route"]["model"] == model and overridden["route"]["effort"] == "high"
for target in ("grok", "muse"):
    rejected = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--dispatch", "worker",
                               "--to", target, "--prompt", "Inspect source.", "--dry-run"],
                              env=environment, capture_output=True, text=True)
    assert rejected.returncode != 0 and "requires an explicit valid --model" in rejected.stderr
for role, kind, tier in (("builder", "worker", "routine"), ("integrator", "worker", "deep"),
                         ("release-engineer", "worker", "routine"), ("critic", "advisor", "frontier"),
                         ("verifier", "reviewer", "frontier"), ("reviewer", "reviewer", "frontier"),
                         ("researcher", "researcher", "light")):
    allocated = allocate("claude", role=role, installed=both)
    assert (allocated["kind"], allocated["workload"]) == (kind, tier), allocated
for bad in ({"role": "verifier", "access": "write"}, {"role": "release-engineer", "access": "write"},
            {"role": "builder", "purpose": "explain"}, {"tier": "main"}, {"installed": set()},
            {"role": "researcher", "tier": "deep"}):
    try:
        allocate("claude", installed=bad.pop("installed", both), **bad)
        raise AssertionError("invalid allocation accepted: %r" % bad)
    except ValueError as error:
        assert "install one" in str(error) or bad, error
for owner in panel.PROVIDERS:
    arguments = panel.native_command(owner, project)
    if owner == "claude":
        import uuid
        native_id = arguments[arguments.index("--session-id") + 1]
        assert str(uuid.UUID(native_id, version=4)) == native_id and uuid.UUID(native_id).version == 4
        resumed_argv = panel.native_command(owner, project, resume=native_id)
        assert "--session-id" not in resumed_argv and resumed_argv[resumed_argv.index("--resume") + 1] == native_id
    else:
        assert "--session-id" not in arguments
    expected = "gpt-6.1-sol" if owner == "codex" else "claude-opus-5-5"
    assert arguments[arguments.index("--model") + 1] == expected, arguments
    assert "--dispatch ROLE" in panel.bootstrap(owner, project) and "--workload light|routine|deep|frontier" in panel.bootstrap(owner, project)
    assert all(trigger in panel.bootstrap(owner, project) for trigger in (
        "security-auditor for risky changes", "verifier before declaring completion", "integrator when merging",
        "performance-engineer when performance needs measurement", "documenter at completion",
        "release-engineer when deploying", "not all by default", "a context apart"))
    assert "--seat" not in panel.bootstrap(owner, project) and "Astra" not in panel.bootstrap(owner, project)
    assert "preferred installed provider" not in panel.bootstrap(owner, project) and \
        "ordered by more weekly quota left" in panel.bootstrap(owner, project)
    assert all(trigger in panel.bootstrap(owner, project) for trigger in (
        "authentication, permissions, secrets, signals, install", "architect for structural decisions",
        "release-engineer when deploying", "never all concurrently by default"))
    assert "control tower: by default delegate" in panel.bootstrap(owner, project)
    assert all(token in panel.bootstrap(owner, project) for token in (
        "one notification per call", "configured parallelism", "same turn", "--continue"))
    assert "exactly one live main" in panel.bootstrap(owner, project) and "trust-boundary" in panel.bootstrap(owner, project)
    assert "The sole main lands its work" in panel.bootstrap(owner, project)
    assert "oms scratch-worktree add --repo . starts detached" in panel.bootstrap(owner, project)
    assert "only when publication is authorized" in panel.bootstrap(owner, project)
    assert "git checkout --no-track -b oms/<task>" in panel.bootstrap(owner, project)
    assert "No upstream is needed" in panel.bootstrap(owner, project)
    assert "pushes the verified SHA to its target" in panel.bootstrap(owner, project)
    assert "concurrent land exits 75" in panel.bootstrap(owner, project) and "retry after" in panel.bootstrap(owner, project)
    assert "worker exit or review handoff is an intermediate step" in panel.bootstrap(owner, project)
    assert "OMS_ROOM_REPO" in panel.bootstrap(owner, project)
    assert "oms room scope" in panel.bootstrap(owner, project) and "--scope PATH" in panel.bootstrap(owner, project)
option_text = "--dangerously-skip-permissions"
assert panel.native_command("claude", project, task=option_text)[-2:] == ["--", option_text]
assert option_text not in panel.native_command("codex", project, task=option_text)[:-1]
pi_argv = panel.native_command("pi", project, task=option_text, room_id="pi-room", member="pi-main")
assert pi_argv[:4] == ["bash", str(panel.ENTRY), "harness", "pi"], pi_argv
assert pi_argv[pi_argv.index("--model") + 1] == "gpt-6.1-sol"
assert "--append-system-prompt" in pi_argv and "--owner codex" in pi_argv[pi_argv.index("--append-system-prompt") + 1]
assert json.dumps(option_text) in pi_argv[-1] and pi_argv[-2] == "--", pi_argv
assert "pi-room" in pi_argv[pi_argv.index("--append-system-prompt") + 1]
try:
    panel.native_command("pi", project, resume="exact-session-id")
    raise AssertionError("Pi history was treated as a Codex native session")
except ValueError as error:
    assert "verified Pi session binding" in str(error)
assert "--owner codex" in panel.watch_shell(project, "pi") and "--owner pi" not in panel.watch_shell(project, "pi")
assert "Codex CLI" in panel.main_missing("pi") and "Pi CLI" in panel.main_missing("pi")
for harness in panel.MAIN_HARNESSES:
    with patch.object(panel, "main_installed", return_value=False):
        try:
            panel._run_native(harness, project)
            raise AssertionError("missing harness started")
        except ValueError as error:
            assert str(error) == panel.main_missing(harness)
with patch.dict(os.environ, environment, clear=True), patch.object(panel.shutil, "which", return_value="/x"), \
        patch.object(panel, "ensure_room", return_value="pi-room"), patch.object(panel, "events", \
            side_effect=lambda repo, *args, **kwargs: "pi-attempt" if args[0] == "start" else "[]") as pi_events, \
        patch.object(panel.room, "join") as pi_join, patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel.panel_host, "herdr_active", return_value=False), patch.object(panel, "update_activity"), \
        patch.object(panel.subprocess, "Popen") as pi_process:
    pi_process.return_value.wait.return_value = pi_process.return_value.poll.return_value = 0
    assert panel._run_native("pi", project, task="fixture") == 0
    assert pi_join.call_args.args[3] == "codex" and pi_join.call_args.args[7] is None
    assert "panel_harness=pi" in next(c.args for c in pi_events.call_args_list if c.args[1] == "start")
    pi_env = pi_process.call_args.kwargs["env"]
    assert pi_env["OMS_PANEL_OWNER"] == pi_env["OMS_AGENT"] == "codex" and pi_env["OMS_PANEL_HARNESS"] == "pi"
    assert "OMS_HOOK_AGENT" not in pi_env




# Stop verification's heartbeat during argument expansion, before its own
# traps exist. A bare background function inherits the delegate's EXIT cleanup
# and used to delete the verifier output and frozen Git snapshot here.
delegate_source = (root / "scripts/peer-delegate.sh").read_text()
verify_functions = "\n".join(re.search(r"^" + name + r"\(\) \{\n.*?^\}",
                                      delegate_source, re.M | re.S).group()
                             for name in ("stop_verify_heartbeat", "run_verify"))
heartbeat_probe = r'''
set -eu
. "$1/scripts/lib/peer-common.sh"
eval "$3"
worktree="$2"
REPO="$2"
verify_out="$2/verify-output"
artifact="$2/artifact"
worker_guard_dir="$2/guard"
mkdir "$worker_guard_dir"
touch "$worker_guard_dir/snapshot"
heartbeat_entered="$2/heartbeat-entered"
heartbeat_release="$2/heartbeat-release"
trap 'touch "$heartbeat_release"; rm -f "$verify_out" "$worker_guard_dir/snapshot"' EXIT
ma_scripts_dir() {
  touch "$heartbeat_entered"
  while [ ! -f "$heartbeat_release" ]; do sleep .01; done
  printf '%s\n' "${MA_LIB_DIR%/*}"
}
verify_clean_env() { :; }
run_verify_with_timeout() {
  local tries=0
  while [ ! -f "$heartbeat_entered" ] && [ "$tries" -lt 500 ]; do
    sleep .01; tries=$((tries + 1))
  done
  test -f "$heartbeat_entered"
}
worker_worktree_require_identity() { return 0; }
delegate_attempt_transition() { return 0; }
worker_execution_boundary_check() { test -f "$worker_guard_dir/snapshot"; }
worker_protected_boundary_check() { worker_execution_boundary_check; }
worker_status=0 worker_attempt_owned=1 WORKER_ACCESS=write
worker_attempt_id=fixture VERIFY_CMD=fixture
run_verify
test "$verify_status" = 0
test -f "$verify_out"
test -f "$worker_guard_dir/snapshot"
'''
heartbeat_temporary = temporary / "verify-heartbeat"
heartbeat_temporary.mkdir()
call(["bash", "-c", heartbeat_probe, "verify-heartbeat", str(root),
      str(heartbeat_temporary), verify_functions])


main_attempts = ended_mains()
import panel_results as saved
_operator_block("native-controls")

# Task bodies keep native admission while only their shared display title is withheld.
for task in ("/review", "../scripts inspect", "Explain " + "/" + "home" + "/fixture/private.py", "x" * 160):
    with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show", side_effect=empty_control), \
            patch.object(panel, "read_field", side_effect=["t", task, "q"]), \
            patch.object(panel, "ensure_room", return_value="fixture-room") as chosen_room, \
            patch.object(panel, "open_native", return_value=0) as task_open, \
            patch.dict(os.environ, environment, clear=True):
        assert panel.interactive(project) == 0
        assert task_open.call_args.kwargs["task"] == task
        expected_title = "x" * 160 if task == "x" * 160 else "Shared work"
        assert chosen_room.call_args.kwargs["title"] == expected_title

# More actions opens ordinary scrollable help, even when the viewport is short.
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show", side_effect=empty_control), \
        patch.object(panel, "read_field", side_effect=["?", "", "q"]), \
        patch.object(panel, "open_native") as help_launch, patch.object(panel, "dispatch") as help_dispatch, \
        patch.object(panel.shutil, "get_terminal_size", return_value=os.terminal_size((32, 12))), \
        patch.object(panel, "terminal_style", return_value=(False, False, False)), \
        patch.dict(os.environ, environment, clear=True), patch("sys.stdout", new=io.StringIO()) as help_text:
    assert panel.interactive(project) == 0
    assert "all shortcuts" in help_text.getvalue() and "Advisor" in help_text.getvalue()
    assert "Finalize" in help_text.getvalue() and "Resume" in help_text.getvalue()
    help_launch.assert_not_called()
    help_dispatch.assert_not_called()

# A lifecycle writer failure cannot kill a running native main or mask its exit.
from unittest.mock import Mock
with patch.object(panel, "managed_session", return_value=False), patch.object(panel, "show", side_effect=empty_control), \
        patch.object(panel, "read_field", return_value="1"), patch.object(panel, "ensure_room", return_value="fixture-room"), \
        patch.object(panel, "open_native", side_effect=panel.NativeShutdown(15)), patch.dict(os.environ, environment, clear=True):
    try:
        panel.interactive(project)
    except panel.NativeShutdown as error:
        assert error.exit_code == 143
    else:
        raise AssertionError("interactive panel swallowed native termination")
uncertain_child = Mock()
uncertain_child.wait.side_effect = KeyboardInterrupt
uncertain_child.poll.return_value = None
uncertain_child.terminate.side_effect = OSError("fixture shutdown unavailable")
with patch.object(panel, "events", return_value="att_fixture") as event_log, \
        patch.object(panel, "safe_label", return_value="Inspect source"), \
        patch.object(panel, "managed_session", return_value=False), \
        patch.object(panel, "ensure_room", return_value="fixture-room"), patch.object(panel.room, "join"), \
        patch.object(panel, "check_legacy_main"), \
        patch.object(panel.subprocess, "Popen", return_value=uncertain_child), patch.dict(os.environ, environment, clear=True):
    try:
        panel.run_native("codex", project)
    except KeyboardInterrupt:
        pass
    assert any("native_shutdown_unconfirmed" in row.args and "blocked" in row.args for row in event_log.call_args_list)
    assert not any("cancelled" in row.args or "done" in row.args for row in event_log.call_args_list)
for action in ("native", "council"):
    process = Mock()
    process.wait.side_effect = [subprocess.TimeoutExpired("fixture", 60), 0]
    process.poll.return_value = 0
    def broken_events(repo, operation, *args, **kwargs):
        if operation == "start":
            return "att_fixture"
        raise ValueError("fixture ledger is unavailable")
    with patch.object(panel, "events", side_effect=broken_events), \
            patch.object(panel, "safe_label", return_value="Inspect source"), \
            patch.object(panel, "managed_session", return_value=False), \
            patch.object(panel, "ensure_room", return_value="fixture-room"), \
            patch.object(panel.room, "join"), \
            patch.object(panel, "check_legacy_main"), \
            patch.object(panel.subprocess, "Popen", return_value=process), \
            patch.dict(os.environ, environment, clear=True):
        status = panel.run_native("codex", project) if action == "native" else \
            panel.council(project, "codex", "heartbeat-council", "Inspect source", "Compare options")
        assert status == 0 and not process.terminate.called and not process.kill.called

# The reviewed writer follows recorded linkage, not attempt recency: late telemetry advances an earlier
# Claude write's updated_at past the Codex write that produced the reviewed patch.
def history_row(attempt, provider, task="task-a"):
    return dict(attempt_id=attempt, task_id=task, provider=provider,
                refs=dict(panel_alias="a", panel_room_id="r", panel_provider=provider, panel_access="write"))
def patch_row(attempt, digest, base="b" * 40):
    return dict(attempt_id=attempt, patch=".oms/artifacts/%s.patch" % attempt, patch_sha256=digest, base_sha=base)
def linked(attempt, digest, base="b" * 40):
    return dict(patch=".oms/artifacts/%s.patch" % attempt, sha256=digest, base=base)
history_rows = [history_row("att_codex", "codex"), history_row("att_claude", "claude")]  # Claude is last
patch_rows = [patch_row("att_codex", "c" * 64), patch_row("att_claude", "d" * 64)]
import panel_dispatch
with patch.object(panel, "events", side_effect=lambda *a, **k: json.dumps(history_rows)), \
        patch.object(panel, "child_environment", return_value={"OMS_ROOM_ID": "r"}), \
        patch.object(panel_dispatch.room, "resolve", return_value="r"), \
        patch.object(panel_dispatch.room, "same_room", return_value=True), \
        patch.object(saved, "_records", return_value=([], patch_rows)):
    _, writer = panel_dispatch.room_history(project, "claude", "task-a", linked("att_codex", "c" * 64))
    assert writer == "codex", writer
    assert allocate("claude", role="reviewer", installed=both, writer=writer)["provider"] == "claude"
    assert panel_dispatch.room_history(project, "claude", "task-a", linked("att_codex", "e" * 64))[1] is None
    assert panel_dispatch.room_history(project, "claude", "task-a", linked("att_codex", "c" * 64, "f" * 40))[1] is None
    assert panel_dispatch.room_history(project, "claude", "task-a", dict(sha256="c" * 64))[1] is None  # digest alone
    assert panel_dispatch.room_history(project, "claude", "task-a")[1] is None  # mixed writers, no linkage
    # Identical bytes from both providers: the candidate's own patch path names its writer, never the first digest.
    patch_rows[:] = [patch_row("att_claude", "c" * 64), patch_row("att_codex", "c" * 64)]
    assert panel_dispatch.room_history(project, "claude", "task-a", linked("att_codex", "c" * 64))[1] == "codex"
    assert panel_dispatch.room_history(project, "claude", "task-a", linked("att_claude", "c" * 64))[1] == "claude"
    patch_rows.append(dict(patch_row("att_claude", "c" * 64), patch=linked("att_codex", "c" * 64)["patch"]))
    assert panel_dispatch.room_history(project, "claude", "task-a", linked("att_codex", "c" * 64))[1] is None
    # A task with one writing provider but no candidate linkage keeps the owner-opposite fallback.
    history_rows[:] = [history_row("att_codex", "codex")]
    _, writer = panel_dispatch.room_history(project, "claude", "task-a")
    assert writer is None, writer
    assert allocate("claude", role="reviewer", installed=both, writer=writer)["provider"] == "codex"

for owner in panel.PROVIDERS:
    plan = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
                           "--launch", owner, "--resume", "exact-session-id", "--dry-run"]))
    assert plan["executes"] is False and "exact-session-id" in plan["argv"]
    assert "--last" not in plan["argv"] and "--continue" not in plan["argv"]
    assert "--dangerously" not in " ".join(plan["argv"])
# With tmux installed, the fixed panel is the default home for bare and --launch runs, also inside
# another tmux session; inline stays an explicit choice.
if shutil.which("tmux") and os.name != "nt":
    for extra, env_extra, layout in (([], {}, "split"), (["--launch", "codex"], {}, "split"),
                                     ([], {"TMUX": "/tmp/other,1,0"}, "split"),
                                     (["--host", "inline", "--launch", "codex"], {}, "inline"),
                                     (["--layout", "inline"], {}, "inline")):
        plan = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project)] + extra + ["--dry-run"],
                               dict(environment, **env_extra)))
        assert plan["layout"] == layout, (extra, env_extra, plan["layout"])

for arguments in (["--resume", "--last"], ["--launch", "codex", "--resume", "../bad"],
                  ["--launch", "codex", "--resume=--last", "--dry-run"],
                  ["--launch", "codex", "--json"], ["--model", "x"],
                  ["--watch", "--launch", "codex"], ["--watch", "--json"],
                  ["--owner", "claude"], ["--count", "1"], ["--watch", "--count", "0"],
                  ["--no-animation"],
                  ["--health", "--launch", "codex"], ["--health", "--dry-run"], ["--health", "--view", "graph"],
                  ["--json", "--view", "compact"], ["--results", "--attention-only"],
                  ["--launch", "codex", "--view", "detail"],
                  ["--finalize", "--dry-run"], ["--retry-delivery"], ["--results", "--notify"], ["--participant", "w1"],
                  ["--position", "top"],
                  ["--council", "--rounds", "3"],
                  ["--dispatch", "advisor", "--access", "write", "--prompt", "change source"],
                  ["--dispatch", "worker", "--access", "write", "--purpose", "implement", "--prompt", "change source"],
                  ["--dispatch", "worker", "--workload", "main", "--prompt", "explain"],
                  ["--dispatch", "verifier", "--access", "write", "--brief-file", "b.md", "--verify", "true"],
                  ["--dispatch", "reviewer", "--continue", "t1", "--brief-file", "b.md"],
                  ["--dispatch", "builder", "--purpose", "implement", "--prompt", "explain"],
                  ["--dispatch", "advisor", "--purpose", "explain", "--prompt", "explain"],
                  ["--dispatch", "worker", "--continue", "t1", "--prompt", "explain"],
                  ["--dispatch", "worker", "--continue", "t1", "--brief-file", "b.md", "--task-id", "t2"],
                  ["--dispatch", "advisor", "--continue", "t1", "--brief-file", "b.md"],
                  ["--continue", "t1", "--brief-file", "b.md"],
                  ["--json", "--dry-run"], []):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel"] + arguments,
                            cwd=str(project), env=environment, capture_output=True, text=True)
    assert result.returncode == 2, (arguments, result)
for bad_reopen in (["--reopen", "--json"], ["--reopen", "--dry-run"], ["--reopen", "--host", "inline"],
                   ["--reopen", "--launch", "codex"]):
    rejected = subprocess.run(["bash", str(panel.ENTRY), "panel", *bad_reopen], cwd=str(project),
                              env=environment, capture_output=True, text=True)
    assert rejected.returncode == 2, rejected
for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--launch", "codex"],
                            cwd=str(project), env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--reopen"], cwd=str(project),
                            env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
    result = subprocess.run(["bash", str(panel.ENTRY), "panel", "--dispatch", "worker", "--prompt", "explain"],
                            cwd=str(project), env=dict(environment, **guard), capture_output=True, text=True)
    assert result.returncode == 2 and "worker cannot" in result.stderr, result
health_state = {"collection": {"ok": True}, "room": {"participants": [], "message_count": 0}}
health_out = io.StringIO()
with patch.object(panel, "read_panel", return_value=(health_state, 0, "codex", None, "auto", False, "")), \
        patch.object(panel, "run_native", side_effect=AssertionError("health launched a native session")), \
        patch.object(panel, "run_dispatch", side_effect=AssertionError("health dispatched a model")), \
        patch.object(panel.sys, "stdout", health_out):
    assert panel.main(["--repo", str(project), "--health", "--json"]) == 0
assert json.loads(health_out.getvalue())["schema"] == 1 and "items" in json.loads(health_out.getvalue())
health_out = io.StringIO()
with patch.dict(os.environ, {}, clear=False), patch.object(panel.room, "status", return_value={"closed": True}), \
        patch.object(panel, "snapshot", return_value=(dict(health_state, room={"closed": True, "participants": [], "message_count": 0}), 0)) as selected, \
        patch.object(panel, "read_panel", side_effect=AssertionError("explicit health room ignored")), \
        patch("panel_metrics.collect", return_value={}), patch.object(panel, "private_capacity", return_value={}), \
        patch.object(panel.sys, "stdout", health_out):
    assert panel.main(["--repo", str(project), "--health", "--room", "health-room", "--json"]) == 0
    assert selected.call_args.kwargs["room_id"] == "health-room"
assert any(item["label"] == "Room closed" and item["status"] == "full" for item in json.loads(health_out.getvalue())["items"])
snapshot = call(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "1", "--owner", "claude"],
                dict(environment, OMS_HARNESS_CHILD="1", NO_COLOR=""))
assert "MAIN " in snapshot and "\033" not in snapshot, snapshot
filtered = call(["bash", str(panel.ENTRY), "panel", "--watch", "--count", "1", "--view", "compact", "--attention-only"],
                dict(environment, OMS_HARNESS_CHILD="1", NO_COLOR=""))
assert "compact" in filtered and "\033" not in filtered, filtered

ledger = project / ".oms/lifecycle/events.jsonl"
# The canonical operator suite also covers the room front door and native open contract.
_operator_block("room-protocol")
import room
import codex_app_notify
# A child can send and acknowledge through its controller's pinned queue even
# when its own room repository path is unavailable. The queue accepts no
# alternate sender, event kind, or room from the client.
import tempfile
import threading
import shutil
def test_room_relay():
    relay_room = room.create(project, "worker-relay", "Sandbox worker relay")
    room.join(project, relay_room, "relay-main", "codex", role="main")
    room.join(project, relay_room, "relay-worker", "codex", role="worker", parent="relay-main")
    room.send(project, relay_room, "relay-main", "relay-worker", "consume through relay", message_id="relay-incoming")
    relay_queue = tempfile.mkdtemp(prefix="oms-room-relay-", dir=str(temporary))
    relay_env = dict(os.environ, OMS_HARNESS_CHILD="1", OMS_HARNESS_DELEGATE_DEPTH="1",
                    OMS_ROOM_ID=relay_room, OMS_ROOM_PARTICIPANT="relay-worker",
                    OMS_ROOM_ADMITTED_PARTICIPANT="relay-worker", OMS_ROOM_RELAY_ALLOWED="1",
                    OMS_ROOM_REPO=str(project))
    with patch.dict(os.environ, relay_env):
        relay_server = room._RoomRelay(relay_queue, project, relay_room, "relay-worker", temporary)
        relay_thread = threading.Thread(target=relay_server.serve_forever, kwargs={"poll_interval": .01})
        relay_thread.daemon = True
        relay_thread.start()
        try:
            deadline = time.monotonic() + 2
            while not os.path.exists(os.path.join(relay_queue, ".ready")) and time.monotonic() < deadline:
                time.sleep(.01)
            assert os.path.exists(os.path.join(relay_queue, ".ready")), "controller relay queue did not start"
            sandbox_env = dict(relay_env, OMS_ROOM_RELAY_QUEUE=relay_queue)
            note = subprocess.run(["bash", str(room.ENTRY), "room", "send", "--repo", "/proc",
                                   "--id", relay_room, "--participant", "relay-worker", "--text", "worker note",
                                   "--json"], env=sandbox_env, capture_output=True, text=True, timeout=5)
            assert note.returncode == 0, note.stderr
            sent_id = json.loads(note.stdout)["id"]
            assert any(row.get("room_event", {}).get("id") == sent_id and
                       row.get("room_event", {}).get("sender") == "relay-worker"
                       for row in room.records(project, relay_room)), "sandboxed note did not reach the room"
            ack = subprocess.run(["bash", str(room.ENTRY), "room", "ack", "--repo", "/proc",
                                  "--id", relay_room, "--participant", "relay-worker", "--message", "relay-incoming",
                                  "--json"], env=sandbox_env, capture_output=True, text=True, timeout=5)
            assert ack.returncode == 0 and json.loads(ack.stdout)["status"] == "consumed", ack.stderr
            assert any(row.get("room_event", {}).get("kind") == "received" and
                       row.get("room_event", {}).get("participant") == "relay-worker" and
                       "relay-incoming" in row.get("room_event", {}).get("messages", [])
                       for row in room.records(project, relay_room)), "sandboxed ack did not reach the room"
            before_refusal = len(room.records(project, relay_room))
            forged = [
                {"action": "send", "room": relay_room, "participant": "relay-main", "to": "relay-worker",
                 "kind": "note", "text": "forged sender"},
                {"action": "send", "room": relay_room, "participant": "relay-worker", "to": "relay-main",
                 "kind": "publication", "text": "forged kind"},
                {"action": "send", "room": "another-room", "participant": "relay-worker", "to": "relay-main",
                 "kind": "note", "text": "forged room"},
                {"action": "send", "room": relay_room, "participant": "relay-worker", "sender": "relay-main",
                 "to": "relay-main", "kind": "note", "text": "forged sender field"},
            ]
            for request in forged:
                try:
                    room._relay_request(relay_queue, request)
                except ValueError:
                    pass
                else:
                    raise AssertionError("relay accepted forged sender, kind, or room request")
            assert len(room.records(project, relay_room)) == before_refusal, "forged relay requests wrote room events"
            oversized = {"action": "send", "room": relay_room, "participant": "relay-worker",
                         "to": "relay-main", "kind": "note", "text": "x" * 4001}
            try:
                room._relay_request(relay_queue, oversized)
            except ValueError as error:
                assert "4000" in str(error), error
            else:
                raise AssertionError("relay accepted an oversized note")
            for index in range(19):
                room._relay_request(relay_queue, {"action": "send", "room": relay_room,
                                    "participant": "relay-worker", "to": "relay-main", "kind": "note",
                                    "text": "rate %d" % index})
            before_rate_refusal = len(room.records(project, relay_room))
            try:
                room._relay_request(relay_queue, {"action": "send", "room": relay_room,
                                    "participant": "relay-worker", "to": "relay-main", "kind": "note",
                                    "text": "over rate"})
            except ValueError as error:
                assert "rate limit" in str(error), error
            else:
                raise AssertionError("relay exceeded its per-minute request bound")
            assert len(room.records(project, relay_room)) == before_rate_refusal, "rate refusal wrote a room event"
        finally:
            relay_server.shutdown()
            relay_thread.join(timeout=2)
            shutil.rmtree(relay_queue)
test_room_relay()
# The relay pins its queue directory and never opens a worker-chosen path to
# stop: swapped components, links, FIFOs and a planted .stop touch nothing outside.
def test_room_relay_paths():
    path_room = room.create(project, "worker-relay-paths", "Relay path safety")
    room.join(project, path_room, "path-main", "codex", role="main")
    room.join(project, path_room, "path-worker", "codex", role="worker", parent="path-main")
    path_env = dict(os.environ, OMS_HARNESS_CHILD="1", OMS_HARNESS_DELEGATE_DEPTH="1",
                    OMS_ROOM_ID=path_room, OMS_ROOM_PARTICIPANT="path-worker",
                    OMS_ROOM_ADMITTED_PARTICIPANT="path-worker", OMS_ROOM_RELAY_ALLOWED="1",
                    OMS_ROOM_REPO=str(project))
    def request_bytes(text):
        return (json.dumps({"action": "send", "room": path_room, "participant": "path-worker",
                            "to": "path-main", "kind": "note", "text": text}) + "\n").encode("utf-8")
    def snapshot(directory):
        return {name: open(os.path.join(directory, name), "rb").read() for name in sorted(os.listdir(directory))
                if os.path.isfile(os.path.join(directory, name)) and not os.path.islink(os.path.join(directory, name))}
    outside = tempfile.mkdtemp(prefix="oms-room-outside-", dir=str(temporary))
    try:
        # A swapped queue path: the pinned directory keeps being served, the new target is never read or written.
        queue = tempfile.mkdtemp(prefix="oms-room-pin-", dir=str(temporary))
        decoy = os.path.join(outside, "req-" + "a" * 32 + ".json")
        with open(decoy, "wb") as handle:
            handle.write(request_bytes("decoy outside the queue"))
        with open(os.path.join(outside, "keep"), "wb") as handle:
            handle.write(b"keep\n")
        before_outside = snapshot(outside)
        with patch.dict(os.environ, path_env):
            pinned = room._RoomRelay(queue, project, path_room, "path-worker", temporary)
            moved = queue + ".moved"
            os.rename(queue, moved)
            os.symlink(outside, queue)
            with open(os.path.join(moved, "req-" + "b" * 32 + ".json"), "wb") as handle:
                handle.write(request_bytes("inside the pinned queue"))
            pinned.handle_pending()
            assert os.path.exists(os.path.join(moved, "res-" + "b" * 32 + ".json")), "pinned queue was not served"
            assert snapshot(outside) == before_outside, "a swapped queue path redirected relay file operations"
            assert not any(row.get("room_event", {}).get("text") == "decoy outside the queue"
                           for row in room.records(project, path_room)), "relay read a request through a swapped path"
            # Links and FIFOs inside the queue are refused without blocking, and only the queue entry goes away.
            os.link(os.path.join(outside, "keep"), os.path.join(moved, "req-" + "c" * 32 + ".json"))
            os.symlink(decoy, os.path.join(moved, "req-" + "d" * 32 + ".json"))
            os.mkfifo(os.path.join(moved, "req-" + "e" * 32 + ".json"))
            worker = threading.Thread(target=pinned.handle_pending)
            worker.daemon = True
            worker.start()
            worker.join(timeout=3)
            assert not worker.is_alive(), "relay blocked on a FIFO request"
            for name in "cde":
                refusal = json.loads(open(os.path.join(moved, "res-" + name * 32 + ".json")).read())
                assert refusal["ok"] is False, refusal
            assert snapshot(outside) == before_outside, "a linked or symlinked request touched files outside the queue"
            # A directory in a request's name is refused on every poll without keeping a descriptor open.
            os.mkdir(os.path.join(moved, "req-" + "f" * 32 + ".json"))
            open_fds = len(os.listdir("/proc/self/fd")) if os.path.isdir("/proc/self/fd") else None
            for unused in range(3):
                pinned.handle_pending()
            assert open_fds is None or len(os.listdir("/proc/self/fd")) == open_fds, "directory requests leaked descriptors"
            os.close(pinned.queue_fd)
        os.unlink(queue)
        shutil.rmtree(moved)
        # Shutdown never opens .stop: a symlink or FIFO planted there is neither followed nor waited on.
        queue = tempfile.mkdtemp(prefix="oms-room-stop-", dir=str(temporary))
        sentinel = os.path.join(outside, "sentinel")
        with open(sentinel, "wb") as handle:
            handle.write(b"sentinel\n")
        with patch.dict(os.environ, path_env):
            stopped = room._RoomRelay(queue, project, path_room, "path-worker", temporary)
            thread = threading.Thread(target=stopped.serve_forever, kwargs={"poll_interval": .01})
            thread.daemon = True
            thread.start()
            os.symlink(sentinel, os.path.join(queue, ".stop"))
            stopped.shutdown()
            thread.join(timeout=3)
            assert not thread.is_alive(), "relay shutdown hung"
        assert open(sentinel, "rb").read() == b"sentinel\n", "relay shutdown followed a .stop symlink"
        os.unlink(os.path.join(queue, ".stop"))
        os.mkfifo(os.path.join(queue, ".stop"))
        server = subprocess.Popen([sys.executable, str(Path(room.__file__).resolve()), "relay-server",
                                   "--repo", str(project), "--id", path_room, "--participant", "path-worker",
                                   "--queue", queue, "--worktree", str(temporary)],
                                  env=path_env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 5
            while not os.path.exists(os.path.join(queue, ".ready")) and server.poll() is None and time.monotonic() < deadline:
                time.sleep(.02)
            assert os.path.exists(os.path.join(queue, ".ready")), "relay server did not start"
            server.terminate()
            server.wait(timeout=5)
        finally:
            if server.poll() is None:
                server.kill()
                server.wait()
            server.stderr.close()
        assert open(sentinel, "rb").read() == b"sentinel\n"
    finally:
        shutil.rmtree(outside, ignore_errors=True)
        shutil.rmtree(queue, ignore_errors=True)
test_room_relay_paths()
# Typed close is authorized by enrollment or creation and retains unread mail.
closed_room = room.create(project, "typed-close", "Close contract", creator="creator")
room.join(project, closed_room, "close-writer", "codex")
room.join(project, closed_room, "close-reader", "claude", role="worker")
room.send(project, closed_room, "close-writer", "close-reader", "Preserve until read", message_id="close-mail")
for who in ("outsider",):
    try:
        room.close(project, closed_room, who)
    except ValueError as error:
        assert "not joined" in str(error)
    else:
        raise AssertionError("nonmember closed the room")
room.close(project, closed_room, "creator")
assert room.status(project, closed_room)["closed"]
assert room.records(project, closed_room)[-1]["room_event"] == {"kind": "closed", "participant": "creator"}
closed_rows = room.records(project, closed_room)
room.close(project, closed_room, "creator")
assert room.records(project, closed_room) == closed_rows, "close is idempotent"
for operation in (lambda: room.join(project, closed_room, "late", "codex"),
                  lambda: room.send(project, closed_room, "close-writer", "close-reader", "Late mail")):
    try:
        operation()
    except ValueError as error:
        assert "room is closed" in str(error)
    else:
        raise AssertionError("closed room accepted new enrollment/mail")
assert room.updates(project, closed_room, "close-reader")["turns"][0]["room_event"]["id"] == "close-mail"
room.acknowledge(project, closed_room, "close-reader", ["close-mail"])
assert not room.status(project, closed_room)["messages"][0]["pending_for"]
listed_rooms = json.loads(call(["bash", str(room.ENTRY), "room", "list", "--repo", str(project), "--json"], environment))
assert next(r for r in listed_rooms["rooms"] if r["id"] == closed_room)["closed"]
member_close = room.create(project, "member-close", "Member close")
room.join(project, member_close, "joined-closer", "codex")
closed_cli = json.loads(call(["bash", str(room.ENTRY), "room", "close", "--repo", str(project),
                              "--id", member_close, "--participant", "joined-closer", "--json"], environment))
assert closed_cli["status"] == "closed"
assert room.status(project, member_close)["closed"]
for env in ({"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": member_close, "OMS_ROOM_PARTICIPANT": "joined-closer"},
            {"OMS_ROOM_ID": member_close, "OMS_ROOM_PARTICIPANT": "foreign-caller"}):
    with patch.dict(os.environ, env):
        try:
            room.close(project, member_close, "joined-closer")
        except ValueError:
            pass
        else:
            raise AssertionError("child or foreign binding closed the room")
# A new participant starts after enrollment, even when history exceeds its first byte budget.
mail_room = room.create(project, "enrollment-mail", "Enrollment mailbox")
room.join(project, mail_room, "mail-writer", "codex")
room.send(project, mail_room, "mail-writer", "all", "Pre-enrollment mail", message_id="old-mail")
legacy_cursor = room.thread_live.updates(project, mail_room, limit=1)["cursor"]
room.join(project, mail_room, "mail-reader", "claude", role="worker")
room.send(project, mail_room, "mail-writer", "mail-reader", "Assignment", message_id="first-mail")
initial_mail = room.updates(project, mail_room, "mail-reader", budget=1)
assert [r["room_event"]["id"] for r in initial_mail["turns"]] == ["first-mail"], "initial mail traversed pre-enrollment history"
assert not room.updates(project, mail_room, "mail-reader", initial_mail["cursor"])["turns"]
assert [r["room_event"]["id"] for r in room.updates(project, mail_room, "mail-reader", legacy_cursor)["turns"]] == ["first-mail"], "old cursors remain readable"
resume_room = room.create(project, "resume-mail", "Resume cutoff")
room.join(project, resume_room, "resume-writer", "codex")
room.join(project, resume_room, "resume-reader", "claude", role="worker")
room.send(project, resume_room, "resume-writer", "resume-reader", "Before resume", message_id="before-resume")
room.append(project, resume_room, {"kind": "leave", "participant": "resume-reader"}, "Participant left")
room.join(project, resume_room, "resume-reader", "claude", role="worker")
room.send(project, resume_room, "resume-writer", "resume-reader", "After resume", message_id="after-resume")
assert [r["room_event"]["id"] for r in room.updates(project, resume_room, "resume-reader")["turns"]] == [
    "after-resume"], "resume replayed mail from before the participant cutoff"
# Default reads are addressed unread mail; explicit cursors remain raw stream deltas.
unread_room = room.create(project, "unread-mail", "Unread mailbox")
room.join(project, unread_room, "unread-a", "codex")
room.join(project, unread_room, "unread-b", "claude", role="worker")
room.join(project, unread_room, "unread-c", "codex", role="worker")
start_cursor = room.updates(project, unread_room, "unread-b")["cursor"]
for number in range(60):
    message_id = "old-%s" % number
    room.send(project, unread_room, "unread-a", "unread-b", "Already consumed", message_id=message_id)
    room.acknowledge(project, unread_room, "unread-b", [message_id])
room.send(project, unread_room, "unread-c", "unread-b", "Direct from second sender", message_id="new-direct")
room.send(project, unread_room, "unread-a", "all", "Broadcast", message_id="new-broadcast")
room.send(project, unread_room, "unread-a", "unread-c", "Other recipient only", message_id="other-only")
before_read = room.records(project, unread_room)
page = room.updates(project, unread_room, "unread-b", budget=1, limit=1)
assert [row["room_event"]["id"] for row in page["turns"]] == ["new-direct"]
assert page["has_more"], "unread pagination ignored a later addressed message"
default_mail = room.updates(project, unread_room, "unread-b")
assert [row["room_event"]["id"] for row in default_mail["turns"]] == ["new-direct", "new-broadcast"]
assert not default_mail["has_more"] and room.records(project, unread_room) == before_read, "reading changed room state"
assert [row["room_event"]["id"] for row in room.updates(project, unread_room, "unread-b")["turns"]] == [
    "new-direct", "new-broadcast"], "a repeated read hid unacknowledged mail"
raw_delta = room.updates(project, unread_room, "unread-b", start_cursor)
raw_rows = list(raw_delta["turns"])
for _ in range(len(before_read)):
    if not raw_delta["has_more"]:
        break
    raw_delta = room.updates(project, unread_room, "unread-b", raw_delta["cursor"])
    raw_rows.extend(raw_delta["turns"])
assert {row["room_event"].get("id") for row in raw_rows} >= {"old-0", "old-59", "new-direct", "new-broadcast"}
other_mail = {row["room_event"]["id"] for row in room.updates(project, unread_room, "unread-c")["turns"]}
assert other_mail == {"new-broadcast", "other-only"}, "direct mail crossed recipients or broadcast was hidden"
room.acknowledge(project, unread_room, "unread-b", ["new-direct", "new-broadcast"])
assert room.updates(project, unread_room, "unread-b")["turns"] == [], "acknowledged mail remained in the default inbox"
try:
    room.updates(project, unread_room, "unread-b", "invalid-cursor")
except ValueError:
    pass
else:
    raise AssertionError("invalid room cursor was accepted")
# A cached hook projection must not move its cursor past mail appended before the scan.
race_room = room.create(project, "first-mail-race", "First delivery race")
room.join(project, race_room, "race-writer", "codex")
room.join(project, race_room, "race-reader", "claude", role="worker")
room.join(project, race_room, "race-other", "codex", role="worker")
room.send(project, race_room, "race-writer", "race-reader", "Consumed", message_id="race-old")
room.acknowledge(project, race_room, "race-reader", ["race-old"])
cached_room = room.project(room.records(project, race_room))
room.send(project, race_room, "race-writer", "race-reader", "New direct", message_id="race-direct")
room.send(project, race_room, "race-writer", "all", "New broadcast", message_id="race-broadcast")
room.send(project, race_room, "race-writer", "race-other", "Other reader", message_id="race-other-only")
race_page = room.updates(project, race_room, "race-reader", budget=1, limit=1, state=cached_room)
assert not race_page["turns"] and race_page["has_more"], "first read classified mail outside its projection"
race_next = room.updates(project, race_room, "race-reader", race_page["cursor"], budget=1, limit=1)
assert [r["room_event"]["id"] for r in race_next["turns"]] == ["race-direct"] and race_next["has_more"], "cursor skipped concurrent mail"
race_tail = room.updates(project, race_room, "race-reader", race_next["cursor"])
assert [r["room_event"]["id"] for r in race_tail["turns"]] == ["race-broadcast"], "cursor skipped broadcast or crossed recipients"
assert {r["room_event"]["id"] for r in room.updates(project, race_room, "race-reader")["turns"]} == {
    "race-direct", "race-broadcast"}, "delivery consumed mail or replayed acknowledged mail"
room.append(project, race_room, {"kind": "leave", "participant": "race-reader"}, "Reader left")
room.send(project, race_room, "race-writer", "all", "While absent", message_id="race-absent")
room.join(project, race_room, "race-reader", "claude", role="worker")
stale_membership = room.updates(project, race_room, "race-reader", state=cached_room)
assert not stale_membership["turns"] and stale_membership["has_more"]
assert not room.updates(project, race_room, "race-reader", stale_membership["cursor"])["turns"], "cached enrollment leaked broadcast to a later join"
assert not room.updates(project, race_room, "race-reader")["turns"], "rejoining replayed mail from the previous enrollment"
single_room = room.create(project, "single-main-room", "Single main admission")
single_owner = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + single_room,
    "--ref", "panel_harness=claude", "--then", "starting", "--then", "working", output=True)
room.join(project, single_room, single_owner, "claude", model="claude-opus-5-5")
single_before = room.records(project, single_room)
for provider in ("claude", "codex"):
    try:
        room.join(project, single_room, "second-main", provider)
    except ValueError as error:
        assert single_owner in str(error) and "--swap-main" in str(error), error
    else:
        raise AssertionError("a second live main joined the room")
assert room.records(project, single_room) == single_before
# Handoff text alone cannot authorize a different participant alongside its live sender.
room.send(project, single_room, single_owner, "all", "SWAP_HANDOFF swap-fixture: bounded handoff", "handoff")
try:
    room.join(project, single_room, "forged-successor", "codex")
except ValueError as error:
    assert "--swap-main" in str(error), error
else:
    raise AssertionError("handoff text authorized an unrelated live main")
# Council records use the main's participant but cannot change its native ownership proof.
single_council = panel.events(project, "start", "--provider", "claude", "--tool", "panel-council",
    "--parent-attempt-id", single_owner, "--ref", "panel_role=advisor",
    "--ref", "panel_room_id=" + single_room, "--ref", "panel_room_participant=" + single_owner,
    "--then", "starting", "--then", "working", "--then", "verifying", "--then", "review", "--then", "done",
    output=True)
assert any(row["attempt_id"] == single_council and row["terminal"] is True
           for row in panel.main_history(project, single_room, single_owner))
# The recorded swap retains the same participant after the exact old attempt released.
panel.events(project, "transition", "--attempt", single_owner, "--state", "blocked", "--then", "abandoned",
             "--reason-code", "main_swapped")
room.swap_main(project, single_room, single_owner, "codex", "gpt-6.1-sol", single_owner, "codex")
swap_record = room.status(project, single_room)["swaps"][-1]
assert swap_record["participant"] == single_owner and swap_record["attempt"] == single_owner
assert swap_record["from"] == "claude" and swap_record["to"] == "codex"
room.join(project, single_room, single_owner, "codex", model="gpt-6.1-sol")
assert len(room.status(project, single_room)["participants"]) == 1
room.join(project, single_room, "terminal-successor", "codex")
# Council-only evidence cannot prove that a joined main released its native attempt.
panel.events(project, "start", "--provider", "codex", "--tool", "panel-council", "--ref", "panel_role=advisor",
    "--ref", "panel_room_id=" + single_room, "--ref", "panel_room_participant=terminal-successor",
    "--then", "starting", "--then", "working", "--then", "verifying", "--then", "review", "--then", "done")
try:
    room.join(project, single_room, "unknown-successor", "claude")
except ValueError as error:
    assert "terminal-successor" in str(error) and "unverified" in str(error), error
else:
    raise AssertionError("missing attempt evidence was treated as terminal")
room.append(project, single_room, {"kind": "leave", "participant": "terminal-successor"}, "Left")
room.join(project, single_room, "unknown-successor", "claude")
# A matching name in another room, or in a prior enrollment, cannot release this main.
enrollment_room = room.create(project, "main-enrollment-room", "Main enrollment evidence")
foreign_room = room.create(project, "main-foreign-room", "Foreign main evidence")
room.join(project, enrollment_room, "owner", "codex")
foreign_main = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + foreign_room,
    "--ref", "panel_room_participant=owner", "--then", "starting", "--then", "cancelled", output=True)
try:
    room.join(project, enrollment_room, "foreign-successor", "codex")
except ValueError as error:
    assert "owner" in str(error) and "unverified" in str(error), error
else:
    raise AssertionError("a terminal main from another room released this main")
# Named enrollments have no attempt ID binding; a later creation timestamp must prove ownership.
time.sleep(1.1)
current_main = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main",
    "--ref", "panel_role=main", "--ref", "panel_room_id=" + enrollment_room,
    "--ref", "panel_room_participant=owner", "--then", "starting", "--then", "cancelled", output=True)
room.join(project, enrollment_room, "current-successor", "codex")
room.append(project, enrollment_room, {"kind": "leave", "participant": "current-successor"}, "Left")
time.sleep(1.1)
room.append(project, enrollment_room, {"kind": "leave", "participant": "owner"}, "Left")
room.join(project, enrollment_room, "owner", "codex")
for same_second in (False, True):
    candidate_rows = room.records(project, enrollment_room)
    # Equal-second timestamps are ambiguous too, even when the old attempt predates the rejoin.
    if same_second:
        candidate_rows[-1]["ts"] = json.loads(panel.events(project, "show", "--attempt", current_main,
                                                           "--json", output=True))["created_at"]
    try:
        room.validate_event(candidate_rows, {"kind": "join", "participant": "stale-successor", "provider": "codex",
                            "role": "main", "label": "Stale successor", "model": None, "owns": []}, "Joined", repo=project)
    except ValueError as error:
        assert "owner" in str(error) and "unverified" in str(error), error
    else:
        raise AssertionError("a pre-rejoin main attempt released the current enrollment")
try:
    room.join(project, enrollment_room, "stale-successor", "codex")
except ValueError as error:
    assert "owner" in str(error) and "unverified" in str(error), error
else:
    raise AssertionError("an old same-room attempt released a rejoined main")
time.sleep(1.1)
panel.events(project, "start", "--provider", "codex", "--tool", "panel-main", "--ref", "panel_role=main",
    "--ref", "panel_room_id=" + enrollment_room, "--ref", "panel_room_participant=owner",
    "--then", "starting", "--then", "cancelled")
enrollment_room = room.rollover(project, enrollment_room, "owner")
room.join(project, enrollment_room, "carried-successor", "codex")
# Native startup's first join binds its participant to an attempt created before enrollment.
native_enrollment = room.create(project, "native-enrollment-room", "Native enrollment binding")
native_owner = panel.events(project, "start", "--provider", "codex", "--tool", "panel-main", "--ref", "panel_role=main",
    "--ref", "panel_room_id=" + native_enrollment, "--then", "starting", "--then", "cancelled", output=True)
room.join(project, native_enrollment, native_owner, "codex")
room.join(project, native_enrollment, "native-successor", "codex")
room.append(project, native_enrollment, {"kind": "leave", "participant": "native-successor"}, "Left")
room.append(project, native_enrollment, {"kind": "leave", "participant": native_owner}, "Left")
room.join(project, native_enrollment, native_owner, "codex")
try:
    room.join(project, native_enrollment, "rebound-successor", "codex")
except ValueError as error:
    assert native_owner in str(error) and "unverified" in str(error), error
else:
    raise AssertionError("rejoining revived the first enrollment's attempt ID binding")
shared = room.create(project, "operator-room", "Shared parser task")
room.join(project, shared, main_attempts["codex"], "codex", model="gpt-6-sol", label="Sol main")
legacy_main_join(project, shared, main_attempts["claude"], "claude", model="claude-opus-5-5", label="Opus main")
for fixture_attempt in main_attempts.values():
    panel.events(project, "usage", "--attempt", fixture_attempt, "--ref", "panel_room_id=" + shared)
room_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_PARTICIPANT=main_attempts["claude"],
                OMS_ROOM_REPO=str(project), OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"])
# Parent identities were intentionally closed by earlier negative cases; use a fresh owner here.
room_owner = call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "start",
                   "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                   "--ref", "panel_room_id=" + shared,
                   "--ref", "panel_model=claude-opus-5-5", "--then", "starting", "--then", "working"]).strip()
legacy_main_join(project, shared, room_owner, "claude", model="claude-opus-5-5", label="Active Opus")
room_env.update(OMS_PANEL_MAIN_ATTEMPT=room_owner, OMS_ROOM_PARTICIPANT=room_owner)
answer = call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
               "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate",
               "--workload", "light", "--prompt", "Inspect value.txt without edits.",
               "--task-id", "room-inspection", "--label", "Inspect the shared parser"],
              {name: value for name, value in room_env.items() if name != "OMS_ROOM_REPO"})
assert "artifact:" in answer, answer
state = room.status(project, shared)
child = next(p for p in state["participants"] if p["role"] == "worker")
assert child["parent"] == room_owner and child["model"] == "gpt-6-luna", child
handoff = next(m for m in state["messages"] if m["sender"] == child["participant"])
assert handoff["recipient"] == room_owner and "parent acceptance pending" in handoff["text"], handoff
assert "inspected the bounded repository task" in handoff["text"], handoff
assert (project / "value.txt").read_text() == "old\n", "room cannot widen worker write access"
# A screened summary must still settle the room call, under the same result message id.
sensitive_summary = "pass" + "word=fixture-value"
# These result fixtures have no supervised child tree; the dispatch-exit fixture below
# exercises the real containment and attempt transition contract.
real_test_settle = panel._settle_dispatch
panel._settle_dispatch = lambda repo, room_id, member, status: None
room.join(project, shared, "screen-call", "codex", "worker", parent=room_owner)
with patch.object(panel, "results", return_value={"rows": [{"calls": [{"answer": sensitive_summary}]}]}):
    panel.deliver_result(project, shared, "screen-call", room_owner, "screen-task", 143)
screened = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-screen-call")
assert "Call exit=143" in screened["text"] and "--results --task-id screen-task" in screened["text"]
assert sensitive_summary not in screened["text"]
missing_summary_call = "call-0000000000000006"
room.join(project, shared, missing_summary_call, "codex", "worker", parent=room_owner)
panel._prepare_dispatch_call(project, shared, missing_summary_call, room_owner, task_id="screen-task")
output_log, output_fd = panel._dispatch_output_log(project, missing_summary_call)
output_child = subprocess.Popen([sys.executable, "-c",
                                 "import sys; print('dispatch stderr marker', file=sys.stderr); sys.exit(2)"],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
output_tail, output_lock = bytearray(), threading.Lock()
tees = [threading.Thread(target=panel._tee_dispatch_output, args=(stream, destination,
                     output_tail, output_lock), daemon=True)
        for stream, destination in ((output_child.stdout, sys.stdout), (output_child.stderr, sys.stderr))]
for tee in tees:
    tee.start()
assert output_child.wait(timeout=5) == 2
for tee in tees:
    tee.join(timeout=5)
panel._save_dispatch_output(output_fd, output_tail)
# Short output reaches the tail while the child is still running, not only at EOF.
live_child = subprocess.Popen([sys.executable, "-c", "import sys, time; sys.stdout.write('live'); sys.stdout.flush(); time.sleep(3)"],
                              stdout=subprocess.PIPE)
live_tail, live_sink = bytearray(), io.StringIO()
live_tee = threading.Thread(target=panel._tee_dispatch_output, args=(live_child.stdout, live_sink, live_tail, threading.Lock()),
                            daemon=True)
live_tee.start()
live_deadline = time.monotonic() + 2.5
while not live_tail and time.monotonic() < live_deadline:
    time.sleep(.05)
assert bytes(live_tail) == b"live" and live_child.poll() is None, (bytes(live_tail), live_child.poll())
live_child.wait(timeout=10)
live_tee.join(timeout=5)
with patch.object(panel, "results", side_effect=AttributeError("invalid patch summary")):
    panel.deliver_result(project, shared, missing_summary_call, room_owner, "screen-task", 2)
fallback = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-" + missing_summary_call)
assert "Call exit=2; parent acceptance pending." in fallback["text"]
assert "dispatch stderr marker" in fallback["text"] and output_log in fallback["text"]
assert (project / output_log).stat().st_mode & 0o777 == 0o600
exit_record = json.loads(panel._read(project, panel._dispatch_call_path(missing_summary_call, True)))
assert exit_record["output_log"] == output_log
zero_summary_call = "call-0000000000000007"
room.join(project, shared, zero_summary_call, "codex", "worker", parent=room_owner)
with patch.object(panel, "results", side_effect=AttributeError("invalid patch summary")):
    panel.deliver_result(project, shared, zero_summary_call, room_owner, "screen-task", 0)
zero_fallback = next(m for m in room.status(project, shared)["messages"] if m["id"] == "result-" + zero_summary_call)
assert "Call exit=0; parent acceptance pending." in zero_fallback["text"]
assert "Result evidence unavailable" in zero_fallback["text"]

import io
from contextlib import redirect_stderr
conflict = io.StringIO()
with redirect_stderr(conflict):
    panel.deliver_result(project, shared, "screen-call", room_owner, "screen-task", 1)
assert "message-id conflict: result-screen-call" in conflict.getvalue()
panel._settle_dispatch = real_test_settle

_operator_block("cross-repository-dispatch")
# A room in A keeps its parent, call and results when the worker executes in B,
# whether the caller selects the room explicitly or inherits it.
other_project = temporary / "dispatch work repo"
subprocess.run(["git", "clone", "-q", "--no-hardlinks", str(project), str(other_project)], check=True)
# A distinct base exposes accidental candidate construction from the state repository.
(other_project / "dispatch-source.txt").write_text("other project\n")
subprocess.run(["git", "-C", str(other_project), "add", "dispatch-source.txt"], check=True)
subprocess.run(["git", "-C", str(other_project), "-c", "user.name=test", "-c", "user.email=test@example.com",
                "commit", "-qm", "other project"], check=True)
other_head = subprocess.check_output(["git", "-C", str(other_project), "rev-parse", "HEAD"], text=True).strip()
(other_project / "dispatch-brief.md").write_text("Task: append new to value.txt.\n")
cross_brief = other_project / "dispatch-brief.md"
verify_log = temporary / "cross-repo-verify"
for dry_run in (False, True):
    before = worker_log.read_bytes()
    request = ["bash", str(panel.ENTRY), "peer-delegate", "--repo", str(other_project),
               "--to", "codex", "--brief-file", str(cross_brief), "--no-verify", "--apply"]
    if dry_run:
        request += ["--dry-run"]
    refused = subprocess.run(request, env=dict(room_env, OMS_PANEL_DISPATCH="1"),
                             capture_output=True, text=True, timeout=30)
    assert refused.returncode != 0 and (
        "--apply cannot land a patch built in %s onto panel state repository %s" % (other_project, project)
    ) in refused.stderr, refused
    assert worker_log.read_bytes() == before, "cross-repository apply launched a worker"
    assert (other_project / "value.txt").read_text() == (project / "value.txt").read_text() == "old\n"
for task_id, explicit_room, writing, continuing in (
        ("cross-repo-explicit", True, False, False),
        ("cross-repo-inherited", False, False, False),
        ("cross-repo-write", True, True, False),
        ("cross-repo-write", False, True, True)):
    request = ["bash", str(panel.ENTRY), "panel", "--repo", str(other_project),
               "--dispatch", "worker", "--owner", "claude", "--workload", "light"]
    if writing:
        request += ["--purpose", "implement", "--access", "write", "--brief-file", cross_brief.name,
                    "--verify", 'git rev-parse --git-common-dir HEAD > "$PANEL_TEST_VERIFY_LOG"',
                    "--continue" if continuing else "--task-id", task_id]
    else:
        request += ["--purpose", "investigate", "--prompt", "Inspect value.txt without edits.", "--task-id", task_id]
    if explicit_room:
        request += ["--room", shared]
    # Write calls run without an explicit wall here so the panel's own default is observed.
    call_env = {key: value for key, value in room_env.items() if not (writing and key == "OMS_PEER_TIMEOUT")}
    call_env["OMS_AGENT"] = "claude"  # Match the fixture main independently of the host provider.
    before_calls = len(worker_log.read_text().splitlines())
    answer = call(request, dict(call_env, PANEL_TEST_MODE="write" if writing else "",
                               PANEL_TEST_VERIFY_LOG=str(verify_log)))
    assert "artifact:" in answer, answer
    calls = [json.loads(line) for line in worker_log.read_text().splitlines()[before_calls:]]
    observed, = [row for row in calls if row["role"] == "worker"]
    assert observed["task_id"] == task_id and observed["parent"] == room_owner, observed
    assert observed["room"] == shared and observed["room_repo"] == str(project), observed
    assert Path(observed["cwd"]) != other_project and Path(observed["cwd"]) != project, observed
    assert observed["git_common"] == str(other_project / ".git"), observed
    assert observed["head"] == other_head, observed
    assert not Path(observed["cwd"]).exists(), "cross-repository worker cleanup failed"
    assert observed["timeout"] == ("40m" if writing else room_env.get("OMS_PEER_TIMEOUT")), observed
    assert observed["verify_timeout"] == ("20m" if writing else room_env.get("OMS_PEER_VERIFY_TIMEOUT")), observed
    if writing:
        reviewer, = [row for row in calls if row["role"] == "reviewer"]
        assert reviewer["head"] == other_head and reviewer["git_common"] != observed["git_common"], reviewer
        assert Path(reviewer["cwd"]) not in (other_project, project, Path(observed["cwd"])), reviewer
        expected_value = "old\n" + "new\n" * (2 if continuing else 1)
        expected_blob = subprocess.check_output(["git", "-C", str(other_project), "hash-object", "--stdin"],
                                                input=expected_value, text=True).strip()
        assert "100644 %s 0\tvalue.txt\n" % expected_blob in reviewer["index"], reviewer
        assert verify_log.read_text().splitlines() == [reviewer["git_common"], other_head], "verifier missed the candidate"
        assert not Path(reviewer["cwd"]).exists(), "cross-repository candidate cleanup failed"
        assert "applied: no (review patch, then: oms patch-land --repo %s --patch " % other_project in answer, answer
        assert "oms patch-land --repo %s --patch " % project not in answer, answer
    state = room.status(project, shared)
    enrolled = room.participant(state, observed["participant"])
    assert enrolled["parent"] == room_owner and enrolled["role"] == "worker", enrolled
    messages = state["messages"]
    assert any(m["recipient"] == observed["participant"] and m["text"].startswith("Assigned:") for m in messages), messages
    assert any(m["sender"] == observed["participant"] and m["recipient"] == room_owner
               and "inspected the bounded repository task" in m["text"] for m in messages), messages
    attempts = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
    attempt = next(a for a in attempts if a.get("task_id") == task_id
                   and a.get("refs", {}).get("panel_room_participant") == observed["participant"])
    assert attempt["parent_attempt_id"] == room_owner and attempt["refs"]["panel_room_id"] == shared, attempt
    assert attempt["refs"]["panel_room_participant"] == observed["participant"], attempt
    report = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results",
                             "--task-id", task_id, "--json"], room_env))
    assert report["rows"] and any(c.get("answer") for row in report["rows"] for c in row.get("calls", [])), report
    assert not json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(other_project),
                               "list", "--json"])), "call attempt split into work repository"
    assert not (other_project / ".oms/artifacts/index.jsonl").exists(), "call artifact split into work repository"
    assert (other_project / "value.txt").read_text() == "old\n", "worker edited the source repository"
    assert (project / "value.txt").read_text() == "old\n", "worker edited the state repository"
cross_brief.unlink()
board = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--results", "--json"], room_env))
assert {row["task_id"] for row in board["rows"]} >= {
    "cross-repo-explicit", "cross-repo-inherited", "cross-repo-write"}, board
for changed in ({"OMS_ROOM_PARTICIPANT": main_attempts["codex"]},
                {"OMS_ROOM_REPO": str(temporary / "missing state repo")},
                {"OMS_PANEL_MAIN_ATTEMPT": main_attempts["codex"]}):
    before = worker_log.read_bytes()
    refused = subprocess.run(request, env=dict(room_env, **changed), capture_output=True, text=True, timeout=30)
    assert refused.returncode != 0 and worker_log.read_bytes() == before, (changed, refused)
tamper = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(other_project), "--room", shared,
    "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate", "--workload", "light",
    "--prompt", "Inspect value.txt without edits.", "--task-id", "cross-repo-guard"],
    env=dict(room_env, PANEL_TEST_MODE="primary-write", PANEL_TEST_PRIMARY=str(other_project / "value.txt"),
             OMS_WORKER_GUARD_STRICT="1"), capture_output=True, text=True, timeout=90)
assert tamper.returncode != 0 and "changed protected state outside its worktree: tracked" in tamper.stderr, tamper
assert "source repository protected surfaces changed: tracked" in tamper.stderr, tamper.stderr
assert (other_project / "value.txt").read_text() == "old\noutside worker\n", "fixture did not exercise source protection"
assert (project / "value.txt").read_text() == "old\n", "source mutation reached the state repository"
(other_project / "value.txt").write_text("old\n")
def finish_fixture_reviews():
    # The fixture main accepts finished calls between independent phases, releasing its running pool slots.
    for row in json.loads(panel.events(project, "list", "--json", output=True)):
        if row.get("parent_attempt_id") == room_owner and row.get("state") == "review":
            panel.events(project, "transition", "--attempt", row["attempt_id"], "--state", "done",
                         "--reason-code", "fixture_complete")
finish_fixture_reviews()
# A dispatch with a shared-plan task ID moves that task for the calling main only: dry-run leaves it ready, the
# claim records the main, and every further worker of the same main reuses the running lease.
plan_cli = ["bash", str(panel.ENTRY), "agent-plan", "--repo", str(project)]
call(plan_cli + ["init", "--goal", "dispatch moves the plan"])
call(plan_cli + ["add", "--id", "cross-repo-plan", "--title", "Plan only in the state repository"])
assert not (other_project / ".oms/plan/tasks.json").exists(), "cross-repo task must exist only in the state repository"
plan_hint_request = ["bash", str(panel.ENTRY), "peer-delegate", "--to", "codex",
                     "--plan-task", "cross-repo-plan", "--prompt", "Inspect value.txt.", "--no-verify", "--dry-run"]
plan_hint = call(plan_hint_request + ["--repo", str(other_project)], dict(room_env, OMS_PANEL_DISPATCH="1"))
assert "plan task cross-repo-plan belongs to panel state repository %s" % project in plan_hint, plan_hint
assert "patch is based on %s's HEAD" % other_project in plan_hint, plan_hint
assert "oms patch-land" not in plan_hint, "cross-repo plan hint suggested unsupported landing: " + plan_hint
same_repo_hint = call(plan_hint_request + ["--repo", str(project)], dict(room_env, OMS_PANEL_DISPATCH="1"))
assert "oms patch-land --repo %s --patch " % project in same_repo_hint and "--plan-task cross-repo-plan" in same_repo_hint, same_repo_hint
call(plan_cli + ["add", "--id", "plan-link", "--title", "Plan-linked dispatch"])
call(plan_cli + ["add", "--id", "plan-link-dependency", "--title", "Unfinished dependency"])
call(plan_cli + ["add", "--id", "plan-link-dependent", "--title", "Blocked plan-linked dispatch",
                 "--depends", "plan-link-dependency"])
plan_dispatch = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                 "--dispatch", "worker", "--owner", "claude", "--purpose", "investigate", "--workload", "light",
                 "--prompt", "Inspect value.txt without edits.", "--task-id", "plan-link", "--label", "Plan linked"]
plan_task = lambda: {k: v for k, v in json.loads(call(plan_cli + ["show", "--id", "plan-link"])).items() if k != "claim_age_s"}
call(plan_dispatch + ["--dry-run"], room_env)
assert plan_task()["state"] == "ready", plan_task()
linked = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert linked.returncode == 0, linked
assert "plan plan-link is ready; panel left it unchanged" in linked.stderr, linked.stderr
assert plan_task()["state"] == "ready", plan_task()
call(plan_cli + ["claim", "--id", "plan-link", "--provider", "claude"], room_env)
held = json.loads(call(plan_cli + ["show", "--id", "plan-link"]))
call(plan_cli + ["start", "--id", "plan-link", "--lease-id", held["lease_id"]])
again = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert again.returncode == 0 and plan_task()["state"] == "running", again
assert "plan plan-link running (lease %s)" % held["lease_id"] in again.stderr, again.stderr
dependent_read = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-link-dependent", "--label", "Read dependency"],
                                env=room_env, capture_output=True, text=True)
assert dependent_read.returncode == 0 and "is ready; panel left it unchanged" in dependent_read.stderr, dependent_read
assert json.loads(call(plan_cli + ["show", "--id", "plan-link-dependent"]))["state"] == "ready"
call(plan_cli + ["add", "--id", "plan-link-owned", "--title", "Already-owned read dispatch"])
call(plan_cli + ["claim", "--id", "plan-link-owned", "--provider", "claude"],
     room_env)
owned = json.loads(call(plan_cli + ["show", "--id", "plan-link-owned"]))
call(plan_cli + ["start", "--id", "plan-link-owned", "--lease-id", owned["lease_id"]])
owned_dispatch = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-link-owned", "--label", "Observe owned"],
                                env=room_env, capture_output=True, text=True)
assert owned_dispatch.returncode == 0, owned_dispatch
assert "plan plan-link-owned running (lease %s)" % owned["lease_id"] in owned_dispatch.stderr, owned_dispatch.stderr
assert json.loads(call(plan_cli + ["show", "--id", "plan-link-owned"]))["lease_id"] == owned["lease_id"]
call(plan_cli + ["add", "--id", "plan-held", "--title", "Held by codex"])
call(plan_cli + ["claim", "--id", "plan-held", "--provider", "codex"],
     dict(environment, OMS_ROOM_PARTICIPANT=main_attempts["codex"]))
other = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-held", "--label", "Held elsewhere"], env=room_env,
                       capture_output=True, text=True)
assert other.returncode == 0 and "another claimant" in other.stderr, other
assert json.loads(call(plan_cli + ["show", "--id", "plan-held"]))["state"] == "claimed"
call(plan_cli + ["add", "--id", "plan-blocked", "--title", "Blocked before write"])
call(plan_cli + ["block", "--id", "plan-blocked", "--reason", "waits on a decision"])
call(plan_cli + ["add", "--id", "plan-ownerless", "--title", "Ownerless claim"])
call(plan_cli + ["claim", "--id", "plan-ownerless", "--provider", "codex"])
(project / "write-brief.md").write_text("Change nothing.\n")
write_plan_dispatch = (plan_dispatch[:plan_dispatch.index("--prompt")] +
                       ["--purpose", "implement", "--access", "write", "--workload", "light",
                        "--brief-file", str(project / "write-brief.md"), "--verify", "true"])
write_link = subprocess.run(write_plan_dispatch + ["--task-id", "plan-link-dependent", "--label", "Claim dependency task"],
                            env=room_env, capture_output=True, text=True)
assert write_link.returncode != 0 and "unfinished dependencies" in write_link.stderr, write_link
assert json.loads(call(plan_cli + ["show", "--id", "plan-link-dependent"]))["state"] == "ready"
thread = project / ".oms/threads" / (shared + ".jsonl")
for refused_id in ("plan-blocked", "plan-held", "plan-ownerless"):
    before_thread, before_provider = thread.read_bytes(), worker_log.read_bytes()
    refused = subprocess.run(write_plan_dispatch + ["--task-id", refused_id, "--label", "Refused write"],
                             env=room_env, capture_output=True, text=True)
    assert refused.returncode != 0 and "reopen or claim it before dispatching a write worker" in refused.stderr, refused
    assert thread.read_bytes() == before_thread, "a refused write dispatch must not join the room"
    assert worker_log.read_bytes() == before_provider, "a refused write dispatch must not call a provider"
# Known unclaimable tasks keep informational read dispatch compatibility.
read_blocked = subprocess.run(plan_dispatch[:-4] + ["--task-id", "plan-blocked", "--label", "Read blocked task"],
                              env=room_env, capture_output=True, text=True)
assert read_blocked.returncode == 0 and "plan plan-blocked is blocked" in read_blocked.stderr, read_blocked
# Standalone known plan tasks have no main whose exact binding can be observed.
call(plan_cli + ["add", "--id", "plan-standalone", "--title", "Standalone write"])
standalone_before = worker_log.read_bytes()
standalone_env = dict(environment)
standalone_env.pop("CLAUDE_CODE_SESSION_ID", None)
standalone_argv = list(write_plan_dispatch)
room_arg = standalone_argv.index("--room")
del standalone_argv[room_arg:room_arg + 2]
standalone = subprocess.run(standalone_argv + ["--task-id", "plan-standalone", "--label", "Refused standalone"],
                            env=standalone_env, capture_output=True, text=True)
assert standalone.returncode != 0 and "no owned main relationship" in standalone.stderr, standalone
assert worker_log.read_bytes() == standalone_before
assert json.loads(call(plan_cli + ["show", "--id", "plan-standalone"]))["state"] == "ready"
# Missing tasks retain the legacy unprotected write path.
missing_before = worker_log.read_bytes()
legacy = subprocess.run(write_plan_dispatch + ["--task-id", "plan-not-recorded", "--label", "Legacy write"],
                        env=dict(room_env, PANEL_TEST_MODE="write"), capture_output=True, text=True)
assert legacy.returncode == 0 and worker_log.read_bytes() != missing_before, legacy
finish_fixture_reviews()
call(plan_cli + ["review", "--id", "plan-link", "--lease-id", held["lease_id"]])
closed = subprocess.run(plan_dispatch, env=room_env, capture_output=True, text=True)
assert closed.returncode == 0 and plan_task()["state"] == "review" and "plan plan-link is review" in closed.stderr, closed
before_thread, before_provider = thread.read_bytes(), worker_log.read_bytes()
review_write = subprocess.run(write_plan_dispatch + ["--task-id", "plan-link", "--label", "Refused review write"],
                              env=room_env, capture_output=True, text=True)
assert review_write.returncode != 0 and "reopen or claim it before dispatching a write worker" in review_write.stderr, review_write
assert thread.read_bytes() == before_thread and worker_log.read_bytes() == before_provider, review_write
(project / "write-brief.md").unlink()  # The later blocking reviewer must see only its source fixture.
hinted = subprocess.run(final_args[:final_args.index("--task-id") + 1] + ["plan-link"] + final_args[final_args.index("--task-id") + 2:],
                        cwd=project, env=environment, capture_output=True, text=True)
assert hinted.returncode == 0 and "agent-plan finish --id plan-link --landed-commit SHA" in hinted.stderr, hinted

_operator_block("continuation-and-lineage")

older_round = {"task_id": "ordered-rounds", "parent_attempt_id": "root", "attempt_id": "older",
               "created_at": "2026-10-01T00:00:00Z", "sequence": 9, "refs": {"panel_role": "worker"}}
newer_round = dict(older_round, attempt_id="newer", created_at="2026-10-02T00:00:00Z", sequence=1)
assert [row["attempt_id"] for row in panel._rework_history(
    [newer_round, older_round], "ordered-rounds", {"root"})] == ["older", "newer"]

import shutil
shutil.rmtree(project / ".oms/plan")  # later board fixtures expect a project with no plan
# A continued worker resumes its native session in a new run and carries a delta preamble; without a stored
# session it falls back to a fresh worker, and another main never reaches it.
session = "0199aaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
stored = home / ".codex/sessions/2026/10/07"
stored.mkdir(parents=True)
(stored / ("rollout-2026-10-07T00-00-00-" + session + ".jsonl")).write_text("{}\n")
worker = ["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch", "worker",
          "--owner", "claude", "--purpose", "investigate", "--to", "codex", "--model", "gpt-6-luna"]
call(worker + ["--prompt", "Inspect value.txt without edits.", "--task-id", "room-continue", "--label", "Inspect"],
     dict(room_env, PANEL_TEST_SESSION=session))
listed = lambda: json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--json"]))
first = next(a for a in listed() if a["task_id"] == "room-continue" and a["refs"].get("panel_role") == "worker")
assert first["refs"]["native_session"] == session, first
brief.write_text("Task: look at value.txt again.\n")
prompt_log = temporary / "continue-prompt.md"
again = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue", "--label", "Look again"],
                       env=dict(room_env, PANEL_TEST_SESSION=session, PANEL_TEST_PROMPT=str(prompt_log)),
                       capture_output=True, text=True)
assert again.returncode == 0 and "artifact:" in again.stdout and "native session" in again.stderr, again
resumed = [json.loads(line) for line in worker_log.read_text().splitlines()][-1]
assert resumed["task_id"] == "room-continue", resumed
assert len(re.findall(r"\btask_id=room-continue\b", again.stdout)) == 1, again.stdout
resume_at = resumed["argv"].index("resume")
assert resumed["argv"][resume_at:resume_at + 3] == ["resume", "--all", session], (resumed, again)
assert resumed["argv"][-1] == "-", (resumed, again)
assert any(arg == 'shell_environment_policy.set.OMS_ROOM_REPO=' + json.dumps(str(project))
           for arg in resumed["argv"]), (resumed, again)
text = prompt_log.read_text()
assert "Your earlier work was on" in text and "New instructions follow." in text and "Task: look at value.txt again." in text, text
rounds = [a for a in listed() if a["task_id"] == "room-continue" and a["refs"].get("panel_role") == "worker"]
assert len(rounds) == 2 and rounds[-1]["refs"]["native_session"] == session, rounds
# One session moves between write-capable roles (worker -> refactorer); a reviewer never resumes it.
refactor = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch",
    "refactorer", "--owner", "claude", "--to", "codex", "--model", "gpt-6-luna", "--access", "write",
    "--brief-file", str(brief), "--verify", "true", "--continue", "room-continue", "--dry-run"],
    env=room_env, capture_output=True, text=True)
assert refactor.returncode == 0 and "native session" in refactor.stderr, refactor
planned = json.loads(refactor.stdout)["argv"]
assert planned[planned.index("--role") + 1] == "refactorer" and planned[planned.index("--resume-session") + 1] == session, planned
fresh = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-inspection"], env=dict(
    room_env, PANEL_TEST_PROMPT=str(prompt_log)), capture_output=True, text=True)
assert fresh.returncode == 0 and "no resumable session" in fresh.stderr, fresh
assert "A fresh worker continues earlier work" in prompt_log.read_text(), prompt_log.read_text()
foreign = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"],
                         env=dict(room_env, OMS_PANEL_MAIN_ATTEMPT=main_attempts["claude"]),
                         capture_output=True, text=True)
assert foreign.returncode != 0 and "identities disagree" in foreign.stderr, foreign
third = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"], env=room_env,
                       capture_output=True, text=True)
assert third.returncode == 0, third
escalated = subprocess.run(worker + ["--brief-file", str(brief), "--continue", "room-continue"], env=room_env,
                          capture_output=True, text=True)
assert escalated.returncode == 0, escalated
# Continuation survives retained results that omit the task and never inlines patch bytes it cannot verify.
import hashlib
last_round = [a for a in listed() if a["task_id"] == "room-inspection" and a["refs"].get("panel_role") == "worker"][-1]
patch_dir = project / ".oms/artifacts"
patch_dir.mkdir(parents=True, exist_ok=True)
(patch_dir / "cont.patch").write_text("diff --git a/value.txt b/value.txt\n+tampered-marker\n")
(patch_dir / "link.patch").symlink_to(project / "value.txt")
good = hashlib.sha256((patch_dir / "cont.patch").read_bytes()).hexdigest()
real_results = panel.results
def continued(rows):
    panel.results = lambda repo, task: {"rows": rows, "truncated": True}
    try:
        return panel.continuation(project, "claude", None, "room-inspection", {"provider": "codex"}, brief, room_env)
    finally:
        panel.results = real_results
def prompt_of(rows):
    out = continued(rows)
    return out[0].read_text()
assert "A fresh worker continues" in prompt_of([]), "empty results must still fall back to a fresh worker"
def with_patch(path, digest):
    return [{"calls": [{"attempt_id": last_round["attempt_id"], "patch": path, "patch_sha256": digest,
                        "exit": 0, "answer": "SAFE-CONTINUATION-SUMMARY",
                        "changes": {"issue": "patch evidence unavailable"}}]}]
real_verified_patch = panel.verified_patch
verified_calls = []
def tracked_verified_patch(repo, call):
    verified_calls.append(call)
    return real_verified_patch(repo, call)
panel.verified_patch = tracked_verified_patch
try:
    successful_text = prompt_of(with_patch(".oms/artifacts/cont.patch", good))
    assert "tampered-marker" in successful_text
    assert "SAFE-CONTINUATION-SUMMARY" in successful_text
    assert len(verified_calls) == 1, verified_calls
    verified_calls[:] = []
    rejected = [
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": 1},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": None},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": "0"},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"exit": True},
        with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0] | {"attempt_id": "other-attempt"},
    ]
    missing_exit = with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0]
    del missing_exit["exit"]
    rejected.append(missing_exit)
    for previous_call in rejected:
        text = prompt_of([{"calls": [previous_call]}])
        assert "tampered-marker" not in text and "cont.patch" not in text, text
        assert "SAFE-CONTINUATION-SUMMARY" not in text, text
        assert verified_calls == [], verified_calls
    older = with_patch(".oms/artifacts/cont.patch", good)[0]["calls"][0]
    latest = dict(older, attempt_id=last_round["attempt_id"], exit=1)
    text = prompt_of([{"calls": [older, latest]}])
    assert "tampered-marker" not in text and "cont.patch" not in text, text
    assert "SAFE-CONTINUATION-SUMMARY" not in text, text
    assert verified_calls == [], verified_calls
finally:
    panel.verified_patch = real_verified_patch

# Failed write calls remain salvage-only unless the sole failure is a verified verification failure.
write_last = dict(last_round, refs=dict(last_round["refs"], panel_access="write", panel_location="worktree:fixture"))
real_history = panel._rework_history
def salvage_error(evidence):
    panel._rework_history = lambda rows, task, parents: [write_last]
    panel.results = lambda repo, task: {"rows": [{"calls": [dict(
        attempt_id=write_last["attempt_id"], patch=".oms/artifacts/cont.patch", patch_sha256=good, **evidence)]}]}
    try:
        panel.continuation(project, "claude", None, "room-inspection", {"provider": "codex"}, brief, room_env)
    except ValueError as error:
        assert "first run oms panel --salvage" in str(error), error
    else:
        raise AssertionError("unverified stopped write call did not require salvage")
    finally:
        panel._rework_history = real_history
        panel.results = real_results
salvage_error({"exit": 1, "state": "failed", "guard": "scope"})
salvage_error({"exit": 124, "state": "timed_out"})
# Producer shape: the attempt carries reason_code; the call row has exit=1 and
# the verifier's exit. Such a write continues with its own verified patch.
failed_verify = dict(write_last, reason_code="verification_failed")
panel._rework_history = lambda rows, task, parents: [failed_verify]
panel.results = lambda repo, task: {"rows": [{"calls": [dict(
    attempt_id=failed_verify["attempt_id"], patch=".oms/artifacts/cont.patch", patch_sha256=good,
    exit=1, verify_exit=1, answer="SAFE-CONTINUATION-SUMMARY")]}]}
try:
    carried = panel.continuation(project, "claude", None, "room-inspection", {"provider": "codex"}, brief, room_env)
    assert "tampered-marker" in carried[0].read_text(), carried[0].read_text()
finally:
    panel._rework_history = real_history
    panel.results = real_results
write_last = failed_verify
salvage_error({"exit": 1, "verify_exit": 125, "state": "failed"})  # verifier never ran
salvage_error({"exit": 1, "verify_exit": 124, "state": "failed"})  # verifier timed out
for path, digest in ((".oms/artifacts/cont.patch", "0" * 64), (".oms/artifacts/link.patch", good)):
    text = prompt_of(with_patch(path, digest))
    assert "tampered-marker" not in text, text
    assert "could not be verified" in text, text

def exercise_main_lineage():
    def main_row(ident, member, owner="claude", state="done", terminal=True):
        return {"attempt_id": ident, "provider": owner, "tool": "panel-main", "state": state,
                "terminal": terminal, "created_at": "2026-10-10T00:00:00Z",
                "refs": {"panel_role": "main", "panel_room_participant": member, "panel_owner": owner}}
    old = main_row("lineage-old", "lineage-stable")
    resumed = main_row("lineage-resumed", "lineage-stable", state="working", terminal=False)
    live_old = main_row("lineage-live-old", "lineage-live")
    live = main_row("lineage-live", "lineage-live", state="working", terminal=False)
    foreign = main_row("lineage-foreign", "lineage-foreign", owner="codex")
    blocked = main_row("lineage-blocked", "lineage-blocked", state="blocked", terminal=False)
    unknown = main_row("lineage-unknown", "lineage-unknown", terminal=None)
    workers = [{"attempt_id": "lineage-worker-%d" % n, "task_id": "lineage-task-%d" % n,
                "parent_attempt_id": old["attempt_id"], "provider": "codex", "state": "working" if n == 11 else "review",
                "terminal": False, "sequence": 1, "prompt": "PRIVATE-LINEAGE-PROMPT",
                "refs": {"panel_role": "worker", "panel_label": "Inspect fixture %d" % n,
                         "prompt": "PRIVATE-LINEAGE-PROMPT"}} for n in range(12)]
    workers[0].update(state="done", terminal=True)
    workers[0]["refs"].update(panel_room_id="lineage-room", panel_room_participant="call-bbbbbbbbbbbbbbbb",
                              native_session="lineage-safe-session")
    from types import SimpleNamespace
    process = SimpleNamespace(pid=987654321)
    name = workers[0]["refs"]["panel_room_participant"]
    # Trusted-controller fixtures; live subreaper exhaustion is covered above.
    with patch.object(panel.sys, "platform", "linux"):
        launch = panel._prepare_dispatch_call(project, "lineage-room", name, "lineage-stable")
    def completion(call):
        panel.dispatch_store.write(project, call["participant"] + "-process.json", {"pid": process.pid})
        receipt = {"launch_sha256": panel._dispatch_exit_record(call, 0)["launch_sha256"],
                   "pid": process.pid, "all_reaped": True, "exit": 0}
        panel.dispatch_store.write(project, call["participant"] + "-supervisor.json", receipt)
        panel._record_dispatch_pending(project, call, 0, "lineage-task-0", {"descendants": []})
        panel._record_dispatch_call(project, "lineage-room", call["participant"], "lineage-stable",
                                    process, "lineage-process-identity", 0)
        return receipt
    receipt = completion(launch)
    settled = dict(workers[0], attempt_id="lineage-settled", task_id="lineage-settled", state="done", terminal=True)
    live_worker = dict(workers[0], attempt_id="lineage-live-worker", task_id="lineage-live-task",
                       parent_attempt_id=live_old["attempt_id"])
    foreign_worker = dict(workers[0], attempt_id="lineage-foreign-worker", task_id="lineage-foreign-task",
                          parent_attempt_id=foreign["attempt_id"])
    rows = [old, resumed, live_old, live, foreign, blocked, unknown] + workers + [settled, live_worker, foreign_worker]
    caller = dict(environment, OMS_PANEL_MAIN_ATTEMPT=resumed["attempt_id"], OMS_ROOM_PARTICIPANT="lineage-stable")
    def continue_task(task, env, provider="codex"):
        out, native, note = panel.continuation(project, "claude", None, task, {"provider": provider}, brief, env)
        try:
            assert "New instructions follow." in out.read_text()
            return native
        finally:
            out.unlink()
    def refuse(task, env):
        try:
            continue_task(task, env)
        except ValueError as error:
            assert "no worker" in str(error), error
        else:
            raise AssertionError("foreign worker was continued")
    def ledger(repo, *args, output=False):
        if args[0] == "start":
            return "lineage-next"
        if args[0] == "show":
            # `show --attempt ID` returns that one attempt, like the real ledger.
            wanted = args[args.index("--attempt") + 1]
            return json.dumps(next((row for row in rows if row.get("attempt_id") == wanted), {}))
        return json.dumps(rows)
    with patch.object(panel, "events", side_effect=ledger), patch.object(panel, "results", return_value={"rows": []}), \
            patch("panel_terminals.process_gone", return_value=True) as gone, \
            patch.object(panel, "_dispatch_group_gone", return_value=True) as group_gone:
        assert panel.lineage_ids(project, "claude", "lineage-stable", "lineage-resumed") == {
            "lineage-old", "lineage-resumed"}
        continue_task("lineage-task-0", caller)
        with patch.object(panel, "session_stored", return_value=True):
            assert continue_task("lineage-task-0", caller) == "lineage-safe-session"
            # A provider fallback swaps the provider after the attempt row was written: the session
            # belongs to the provider that ran, so only that provider resumes it.
            workers[0]["refs"]["panel_provider"] = "claude"
            assert continue_task("lineage-task-0", caller) is None
            assert continue_task("lineage-task-0", caller, "claude") == "lineage-safe-session"
            del workers[0]["refs"]["panel_provider"]
            def refuse_exit():
                try:
                    continue_task("lineage-task-0", caller)
                except ValueError as error:
                    assert "historical worker exit is unconfirmed" in str(error), error
                else:
                    raise AssertionError("unconfirmed historical worker resumed")
            for state, terminal in (("working", False), ("review", False), ("done", None)):
                workers[0].update(state=state, terminal=terminal)
                refuse_exit()
            workers[0].update(state="done", terminal=True)
            # Schema 2 has no PID/session fields: only bound exhaustion is authority.
            with patch("panel_terminals.process_gone", return_value=False), \
                    patch.object(panel, "_dispatch_group_gone", return_value=False):
                assert continue_task("lineage-task-0", caller) == "lineage-safe-session"
            receipt_name = name + "-supervisor.json"
            for invalid in (dict(receipt, all_reaped=False), dict(receipt, exit=None), dict(receipt, exit=True),
                            dict(receipt, pid=process.pid + 1), dict(receipt, launch_sha256="0" * 64)):
                panel.dispatch_store.write(project, receipt_name, invalid)
                refuse_exit()
            private = panel.dispatch_store.store(project)
            (private / receipt_name).unlink()
            refuse_exit()
            panel.dispatch_store.write(project, receipt_name, receipt)
            pending_name = name + "-pending.json"
            pending = panel.dispatch_store.read(project, pending_name)
            for invalid in ({}, dict(pending, task_id="../foreign"), dict(pending, launch_sha256="0" * 64)):
                panel.dispatch_store.write(project, pending_name, invalid)
                refuse_exit()
            (private / pending_name).unlink()
            refuse_exit()
            panel.dispatch_store.write(project, pending_name, pending)
            with patch.object(panel, "_dispatch_call_record", return_value=None):
                refuse_exit()
            anchor = panel.dispatch_store.read(project, name + "-launch.json")
            member = {"participant": name, "parent": "lineage-stable"}
            for invalid in (dict(launch, schema=True), dict(launch, schema=3), dict(launch, nonce=None),
                            dict(launch, backend="unknown"), dict(launch, pid=process.pid),
                            {key: value for key, value in launch.items() if key != "nonce"}):
                with patch.object(panel.dispatch_store, "read", return_value=dict(anchor, call=invalid)), \
                        patch.object(panel, "_read", return_value=panel.dispatch_store.encoded(invalid)):
                    assert panel._dispatch_call_record(project, "lineage-room", member) is None
                    refuse_exit()
            assert panel._dispatch_call_record(project, "other-room", member) is None
            assert panel._dispatch_call_record(project, "lineage-room", dict(member, parent="other-main")) is None
            # Legacy records retain all liveness refusals, and need private proof too.
            legacy_name = "call-cccccccccccccccc"
            legacy = {"schema": 1, "kind": "oms-panel-call", "room": "lineage-room", "participant": legacy_name,
                      "parent": "lineage-stable", "pid": process.pid, "identity": "lineage-process-identity",
                      "session": True}
            panel._write(project, panel._dispatch_call_path(legacy_name), legacy)
            legacy_anchor = {"call": legacy, "generation": panel.dispatch_store.generation(
                project / panel._dispatch_call_path(legacy_name))}
            workers[0]["refs"]["panel_room_participant"] = legacy_name
            refuse_exit()
            if os.name == "posix":
                panel.dispatch_store.write(project, legacy_name + "-launch.json", legacy_anchor)
                refuse_exit()
                legacy_receipt = completion(legacy)
                with patch.object(panel, "_dispatch_session_gone", return_value=True) as session_gone:
                    assert continue_task("lineage-task-0", caller) == "lineage-safe-session"
                    gone.return_value = False
                    refuse_exit()
                    gone.return_value = True
                    group_gone.return_value = False
                    refuse_exit()
                    group_gone.return_value = True
                    session_gone.return_value = False
                    refuse_exit()
                    session_gone.return_value = True
                    with patch("panel_terminals.process_gone", side_effect=PermissionError("identity unavailable")):
                        refuse_exit()
                    panel.dispatch_store.write(project, legacy_name + "-supervisor.json", receipt)
                    refuse_exit()
                    panel.dispatch_store.write(project, legacy_name + "-supervisor.json", legacy_receipt)
                    assert continue_task("lineage-task-0", caller) == "lineage-safe-session"
            workers[0]["refs"]["panel_room_participant"] = name
        refuse("lineage-task-0", dict(caller, OMS_PANEL_MAIN_ATTEMPT="lineage-live", OMS_ROOM_PARTICIPANT="lineage-live"))
        refuse("lineage-live-task", caller)
        refuse("lineage-foreign-task", caller)
        assert not panel.lineage_ids(project, "claude", "wrong-participant", "lineage-resumed")
        rows.append(dict(old))
        assert not panel.lineage_ids(project, "claude", "lineage-stable", "lineage-resumed"), "duplicate owner evidence"
        rows.pop()
        rows.append(main_row("lineage-ambiguous", "lineage-stable", state="working", terminal=False))
        assert not panel.lineage_ids(project, "claude", "lineage-stable", "lineage-resumed"), "multiple live owners"
        rows.pop()
        resumed.update(state="done", terminal=True)
        successor = main_row("lineage-next", "lineage-next", state="working", terminal=False)
        successor["created_at"] = "2026-10-11T00:00:00Z"
        rows.append(successor)
        assert panel.lineage_ids(project, "claude", "lineage-next", "lineage-next") == {
            "lineage-old", "lineage-resumed", "lineage-next"}
        continue_task("lineage-task-0", dict(caller, OMS_PANEL_MAIN_ATTEMPT="lineage-next", OMS_ROOM_PARTICIPANT="lineage-next"))
        later = main_row("lineage-later", "lineage-later")
        later["created_at"] = "2026-10-12T00:00:00Z"
        rows.append(later)
        assert "lineage-later" not in panel.lineage_ids(project, "claude", "lineage-next", "lineage-next")
        captured = []
        def native_context(*args, **kwargs):
            captured.append(args[7].read_text())
            return real_native_command(*args, **kwargs)
        real_native_command = panel.native_command
        real_popen = panel.subprocess.Popen
        with patch.dict(os.environ, environment, clear=True), patch.object(panel, "main_installed", return_value=True), \
                patch.object(panel.room, "join"), patch.object(panel, "managed_session", return_value=False), \
                patch.object(panel, "update_activity"), patch.object(panel, "native_command", side_effect=native_context), \
                patch.object(panel.subprocess, "Popen") as process:
            process.return_value.wait.return_value = process.return_value.poll.return_value = 0
            process.side_effect = lambda argv, *a, **kw: (process.return_value
                if argv[0] in {"codex", "claude"} or argv[:3] == ["bash", str(panel.ENTRY), "harness"]
                else real_popen(argv, *a, **kw))
            for harness in panel.MAIN_HARNESSES:
                owner = panel.main_owner(harness)
                for row in (old, resumed, successor):
                    row["provider"] = row["refs"]["panel_owner"] = owner
                foreign["provider"] = foreign["refs"]["panel_owner"] = "codex" if owner == "claude" else "claude"
                assert panel._run_native(harness, project, task="Lineage fixture task", room_id="lineage-room") == 0
                argv = process.call_args.args[0]
                instructions = (argv[-1] if harness == "codex" else argv[argv.index("--append-system-prompt") + 1])
                assert "lineage-task-11" in instructions and "Unsettled lineage workers (data): " in instructions, argv
                assert "PRIVATE-LINEAGE-PROMPT" not in instructions
                assert sum(arg.count("Lineage fixture task") for arg in argv) == 1, argv
        summary = captured[0].split("Unsettled lineage workers (data): ", 1)[1].splitlines()[0]
        calls = json.loads(summary)
        assert len(calls) == 10 and any(c["task_id"] == "lineage-task-11" for c in calls), calls
        assert {c["state"] for c in calls} == {"review", "working"}, calls
        assert all(set(c) == {"task_id", "role", "state", "label"} for c in calls), calls
        assert not {"lineage-settled", "lineage-live-task", "lineage-foreign-task"} & {c["task_id"] for c in calls}
        assert "PRIVATE-LINEAGE-PROMPT" not in captured[0], captured[0]
        assert next(c for c in calls if c["task_id"] == "lineage-task-11")["label"] == "Inspect fixture 11", calls
        assert "--continue TASK_ID" in captured[0]
    print("main lineage: schema-2/verified legacy continuation, live/foreign/unknown refusal and bounded prompt-free context passed")

exercise_main_lineage()
# A status message is a declaration: no mail, no unread, latest per sender kept past the 12-message window;
# unread counts ignore participants who left.
declared = room.create(project, title="Declared status")
room.join(project, declared, "decl-main", "claude", model="claude-opus-5-5", owns=["value.txt"])
room.join(project, declared, "decl-peer", "codex", role="worker", parent="decl-main")
room.join(project, declared, "decl-other", "codex", role="worker", parent="decl-main")
room.send(project, declared, "decl-main", "all", "Parsing the config", kind="status")
for guard in ({"OMS_HARNESS_CHILD": "1"}, {"OMS_HARNESS_DELEGATE_DEPTH": "1"}):
    with patch.dict(os.environ, dict(guard, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-peer")):
        for message_id in ("result-decl-peer", "result-decl-other"):
            before_child_result = len(room.records(project, declared))
            try:
                room.send(project, declared, "decl-peer", "decl-main", "Call exit=0;", kind="handoff",
                          message_id=message_id)
            except ValueError as error:
                assert "result handoffs are written by the panel, not by a worker" in str(error), error
            else:
                raise AssertionError("a worker wrote a panel result handoff")
            assert len(room.records(project, declared)) == before_child_result
        # Nor can it pre-claim the panel's judgment notice or hide its mail from the parent.
        for forged in ({"message_id": "judgment-decl-peer"}, {"quiet": True}):
            try:
                room.send(project, declared, "decl-peer", "decl-main", "All good", **forged)
            except ValueError as error:
                assert "written by the panel, not by a worker" in str(error), error
            else:
                raise AssertionError("a worker forged a panel notice")
        child_note = room.send(project, declared, "decl-peer", "decl-main", "Scoped worker observation")
    room.acknowledge(project, declared, "decl-main", [child_note["id"]])
# Outside workers too, a judgment id names its own call and only a result handoff is quiet.
for forged, reason in (({"message_id": "judgment-decl-other"}, "judgment-<participant> ids are reserved"),
                       ({"quiet": True}, "only a call's result handoff can be quiet")):
    try:
        room.send(project, declared, "decl-peer", "decl-main", "Panel-shaped note", **forged)
    except ValueError as error:
        assert reason in str(error), error
    else:
        raise AssertionError("a panel notice id or quiet flag was accepted on ordinary mail")
room.join(project, declared, "decl-unparented", "codex", role="worker")
with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_ID": declared,
                             "OMS_ROOM_PARTICIPANT": "decl-unparented"}):
    try:
        room.send(project, declared, "decl-unparented", "decl-main", "Missing parent")
    except ValueError as error:
        assert "a worker can only message its parent <missing>" in str(error), error
    else:
        raise AssertionError("a worker without an admitted parent sent a message")
room.append(project, declared, {"kind": "leave", "participant": "decl-unparented"}, "Left")
room.send(project, declared, "decl-peer", "decl-main", "Call exit=3; parent acceptance pending.\nignored", kind="handoff",
          message_id="result-decl-peer")
# Another call cannot settle a sibling by sending a result under its name, and a plain note is no result.
try:
    room.send(project, declared, "decl-peer", "decl-main", "Call exit=0;", kind="handoff", message_id="result-decl-other")
except ValueError as error:
    assert "reserved" in str(error), error
else:
    raise AssertionError("a call sent a result under a sibling's name")
legacy_main_join(project, declared, "decl-elsewhere", "claude", model="claude-opus-5-5")
try:
    room.send(project, declared, "decl-other", "decl-elsewhere", "Call exit=0;", kind="handoff", message_id="result-decl-other")
except ValueError as error:
    assert "to its parent" in str(error), error
else:
    raise AssertionError("a call's result reached a main that is not its parent")
room.append(project, declared, {"kind": "leave", "participant": "decl-elsewhere"}, "Left")
room.acknowledge(project, declared, "decl-main", ["result-decl-peer"])
for number in range(13):
    room.send(project, declared, "decl-peer", "decl-main", "chatter %s" % number)
    room.acknowledge(project, declared, "decl-main", [room.status(project, declared)["messages"][-1]["id"]])
declared_state = room.status(project, declared)
assert declared_state["pending_count"] == 0 and declared_state["statuses"]["decl-main"]["text"] == "Parsing the config", declared_state
assert all(not m["targets"] for m in declared_state["messages"] if m["message_kind"] == "status") and declared_state["statuses"]["decl-main"]["seq"], declared_state
assert not any(t.get("room_event", {}).get("message_kind") == "status" for t in room.updates(project, declared, "decl-peer")["turns"])
room.send(project, declared, "decl-peer", "decl-main", "unread before title change", message_id="before-title")
# Updating a live work title does not rejoin, consume mail or change native identity.
room.bind(project, declared, "decl-main", "declared-title-native")
before_description = room.status(project, declared)
main_before = deepcopy(room.participant(before_description, "decl-main"))
assert "label_override" not in main_before  # New code still reads logs written before describe existed.
mail_before = room.records(project, declared)
cursor_before = room.updates(project, declared, "decl-main", budget=65536, limit=200)["cursor"]
unbound_room_env = dict(environment, OMS_ROOM_ID="", OMS_ROOM_PARTICIPANT="")
with patch.dict(os.environ, unbound_room_env, clear=True):
    room.describe(project, declared, "decl-main", "Improve result navigation")
    after_description = room.status(project, declared)
    main_after = room.participant(after_description, "decl-main")
    assert {key: value for key, value in main_after.items() if key not in {"label", "label_override"}} == {
        key: value for key, value in main_before.items() if key != "label"}
    assert main_after["label"] == main_after["label_override"] == "Improve result navigation"
    assert main_after["seq"] == main_before["seq"] and main_after["joined_at"] == main_before["joined_at"]
    assert main_after["initial_seq"] == main_before["initial_seq"]
    assert after_description["pending_count"] == before_description["pending_count"] == 1
    assert after_description["statuses"] == before_description["statuses"]
    assert after_description["messages"] == before_description["messages"]
    assert after_description["statuses"]["decl-main"]["text"] == "Parsing the config"
    assert [room.visible(r, "decl-main", main_before["seq"]) for r in mail_before] == [
        room.visible(r, "decl-main", main_after["seq"]) for r in mail_before]
    delta_after_description = room.updates(project, declared, "decl-main", cursor_before,
                                           budget=65536, limit=200)
    assert not delta_after_description["turns"] and delta_after_description["cursor"] != cursor_before
    written_description = room.records(project, declared)[-1]["room_event"]
    assert written_description == {"kind": "describe", "participant": "decl-main",
                                   "label": "Improve result navigation"}
    repeated_rows = len(room.records(project, declared))
    room.describe(project, declared, "decl-main", "Improve result navigation")
    assert len(room.records(project, declared)) == repeated_rows
    title_args = ["bash", str(panel.ENTRY), "room", "describe", "--repo", str(project), "--id", declared,
                  "--participant", "decl-main", "--label", "Review result navigation"]
    bound_main_env = dict(environment, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-main")
    call(title_args, bound_main_env)
    assert room.participant(room.status(project, declared), "decl-main")["label"] == "Review result navigation"
    cli_rows = len(room.records(project, declared))
    call(title_args, bound_main_env)
    assert len(room.records(project, declared)) == cli_rows
    for extra in (["--owns", "scripts/lib"], ["--native-session", "native-id"],
                  ["--model", "gpt-6-astra"], ["--parent", "other-main"]):
        rejected = subprocess.run(title_args + extra, env=unbound_room_env, capture_output=True, text=True)
        assert rejected.returncode != 0, (extra, rejected.stdout, rejected.stderr)
    for wrong_binding in (dict(environment, OMS_ROOM_ID=declared, OMS_ROOM_PARTICIPANT="decl-peer"),
                          dict(environment, OMS_ROOM_ID="another-room", OMS_ROOM_PARTICIPANT="decl-main"),
                          dict(environment, OMS_ROOM_ID=declared),
                          dict(environment, OMS_ROOM_PARTICIPANT="decl-main")):
        rejected = subprocess.run(title_args, env=wrong_binding, capture_output=True, text=True)
        assert rejected.returncode != 0, (wrong_binding, rejected.stdout, rejected.stderr)
    rows_for_description = room.records(project, declared)
    description = {"kind": "describe", "participant": "decl-main", "label": "New title"}
    sensitive_label = "pass" + "word=fixture-value"
    for altered in (dict(description, label=""), dict(description, label="x" * 121),
                    dict(description, label="bad\nlabel"), dict(description, label=None),
                    dict(description, consumer="0" * 32), dict(description, owns=[]),
                    dict(description, model="gpt-6-astra"), dict(description, parent="other-main"),
                    dict(description, initial_seq="1"), dict(description, label=sensitive_label),
                    dict(description, participant="decl-peer"), dict(description, participant="missing")):
        try:
            room.validate_event(rows_for_description, altered, "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("invalid description accepted: %r" % altered)
    for malformed in (dict(description, label=None), dict(description, owns=[]),
                       dict(description, label="x" * 121),
                       {"kind": "describe", "participant": "decl-main"}):
        try:
            room.project(rows_for_description + [{"seq": rows_for_description[-1]["seq"] + 1, "room_event": malformed}])
        except ValueError:
            pass
        else:
            raise AssertionError("malformed persisted description accepted")
    left_rows = deepcopy(rows_for_description)
    left_rows.append({"seq": left_rows[-1]["seq"] + 1, "role": "note",
                      "room_event": {"kind": "leave", "participant": "decl-main"}})
    closed_rows = deepcopy(rows_for_description)
    closed_rows.append({"seq": closed_rows[-1]["seq"] + 1, "role": "closed", "text": "closed"})
    for invalid_rows in (left_rows, closed_rows):
        try:
            room.validate_event(invalid_rows, description, "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("describe accepted a left main or closed room")
    for actor in ({"OMS_HARNESS_CHILD": "1", "OMS_HARNESS_DELEGATE_DEPTH": "0",
                   "OMS_ROOM_PARTICIPANT": "decl-peer", "OMS_ROOM_ID": declared},
                  {"OMS_HARNESS_DELEGATE_DEPTH": "1", "OMS_ROOM_PARTICIPANT": "decl-peer",
                   "OMS_ROOM_ID": declared},
                  {"OMS_ROOM_PARTICIPANT": "decl-peer", "OMS_ROOM_ID": declared},
                  {"OMS_ROOM_PARTICIPANT": "decl-main", "OMS_ROOM_ID": "another-room"},
                  {"OMS_ROOM_PARTICIPANT": "decl-main"}, {"OMS_ROOM_ID": declared}):
        with patch.dict(os.environ, actor):
            try:
                room.validate_event(rows_for_description, description, "Work description updated")
            except ValueError:
                pass
            else:
                raise AssertionError("description widened actor authority")
    with patch.dict(os.environ, {"OMS_HARNESS_CHILD": "1", "OMS_ROOM_PARTICIPANT": "decl-peer",
                                 "OMS_ROOM_ID": declared}):
        try:
            room.validate_event(rows_for_description, dict(description, participant="decl-peer"),
                                "Work description updated")
        except ValueError:
            pass
        else:
            raise AssertionError("worker described itself")
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": "decl-main", "OMS_ROOM_ID": declared}):
        assert room.validate_event(rows_for_description, description, "Work description updated") is True
    before_bad_append = len(room.records(project, declared))
    try:
        room.append(project, declared, dict(description, consumer="0" * 32), "Work description updated")
    except ValueError:
        pass
    else:
        raise AssertionError("locked append accepted identity injection")
    assert len(room.records(project, declared)) == before_bad_append
    closed_room = room.create(project, "describe-closed", "Closed title fixture")
    room.join(project, closed_room, "closed-main", "codex")
    room.command(project, "close", "--id", closed_room)
    try:
        room.describe(project, closed_room, "closed-main", "Closed update")
    except ValueError as error:
        assert "closed" in str(error)
    else:
        raise AssertionError("describe appended to a closed room")
    title_state = {"room": after_description, "attempts": {"recent": [{"attempt_id": "decl-attempt", "state": "working",
        "panel": {"role": "main", "room_id": declared, "room_participant": "decl-main",
                  "label": "Stale launch label", "model": "claude-opus-5-5"}}]}}
    from room_view import nodes
    shown_main = next(m for m in nodes(title_state) if m["participant"] == "decl-main")
    assert shown_main["title"] == "Improve result navigation" and shown_main["consumer"] == main_before["consumer"]
    assert shown_main["model"] == shown_main["model_at_launch"] == "claude-opus-5-5"
    legacy_title = deepcopy(title_state)
    legacy_member = room.participant(legacy_title["room"], "decl-main")
    legacy_member.pop("label_override")
    legacy_member["owns"] = ["scope-only-update"]
    assert next(m for m in nodes(legacy_title) if m["participant"] == "decl-main")["title"] == "Stale launch label"
    for title_view in ("tree", "graph"):
        titled = render(title_state, "claude", 120, 40, view=title_view, navigation={"dismissed": True})
        assert "Improve result navigation" in titled and "Now: Parsing the config" in titled and "Opus" in titled, titled
        long_title = deepcopy(title_state)
        room.participant(long_title["room"], "decl-main")["label_override"] = "Now: " + "work title " * 10
        for columns, rows in ((120, 40), (76, 40), (28, 16)):
            bounded = render(long_title, "claude", columns, rows, view=title_view, navigation={"dismissed": True})
            assert all(display_width(line) <= columns for line in bounded.splitlines()) and len(bounded.splitlines()) <= rows
            if columns >= 76:
                assert "Now: Parsing the config" in bounded, bounded
# Finish the title fixture's extra unread mail before the original leave/count checks.
room.acknowledge(project, declared, "decl-main", ["before-title"])
declared_results = room.status(project, declared)["call_results"]
assert declared_results["decl-peer"]["exit"] == 3 and set(declared_results["decl-peer"]) == {"exit", "ts", "seq", "read"}, declared_results
assert "decl-other" not in declared_results, declared_results
room.append(project, declared, {"kind": "leave", "participant": "decl-other"}, "Left")
assert not any(m["id"] == "result-decl-peer" for m in room.status(project, declared)["messages"]), "result left the 12-message window"
room.send(project, declared, "decl-main", "all", "mail for the worker")
assert room.status(project, declared)["pending_count"] == 1
room.append(project, declared, {"kind": "leave", "participant": "decl-peer"}, "Left")
assert room.status(project, declared)["pending_count"] == 0
# Control-window dispatch resolves the chosen provider's active main, rather
# than the first main ever joined, and refuses zero or ambiguous ownership.
control_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project))
control_launch = {}
control_codex = {"provider": "codex", "binary": "codex", "installed": True, "models": ["gpt-6-sol"],
                 "catalog_models": ["gpt-6-sol", "gpt-6.1-sol", "gpt-6.2-sol", "gpt-6-astra"],
                 "access": ["read", "write"], "exact_model_override": True}
control_claude = {"provider": "claude", "binary": "claude", "installed": True,
                  "models": ["claude-haiku-5-5"], "access": ["read", "write"],
                  "exact_model_override": True}
real_popen = panel.subprocess.Popen
def observe_control_launch(argv, *args, **kwargs):
    if (argv[:3] == [sys.executable, "-c", "pass"]
            or argv[5:8] == [sys.executable, "-c", "pass"]):
        control_launch.update(env=kwargs["env"])
    return real_popen(argv, *args, **kwargs)
with patch.dict(os.environ, control_env, clear=True), \
        patch.object(panel, "provider_catalog", return_value=[control_codex, control_claude]), \
        patch.object(panel.shutil, "which", return_value="/fixture/provider"), \
        patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
        patch.object(panel.subprocess, "Popen", side_effect=observe_control_launch):
    assert panel.dispatch(project, "claude", "worker", "light", "read", "investigate",
                          prompt="Inspect the bounded source", task_id="control-owner", label="Control dispatch",
                          alias="B7") == 0
    assert control_launch["env"]["OMS_PANEL_ALIAS"] == "B7"
    newest = room.status(project, shared)["participants"][-1]
    assert newest["parent"] == room_owner
    live_council = panel.events(project, "start", "--provider", "claude", "--tool", "panel-council",
        "--parent-attempt-id", room_owner, "--ref", "panel_role=advisor", "--ref", "panel_room_id=" + shared,
        "--ref", "panel_room_participant=" + room_owner, "--then", "starting", "--then", "working", output=True)
    try:
        assert any(row["attempt_id"] == live_council and row["terminal"] is False
                   for row in panel.main_history(project, shared, room_owner))
        with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner, "OMS_PANEL_MAIN_ATTEMPT": room_owner}):
            assert panel.dispatch(project, "claude", "worker", "light", "read", "investigate",
                                  prompt="Inspect source while the council runs", task_id="council-live-dispatch") == 0
        assert control_launch["env"]["OMS_PANEL_MAIN_ATTEMPT"] == room_owner
        assert room.status(project, shared)["participants"][-1]["parent"] == room_owner
    finally:
        panel.events(project, "transition", "--attempt", live_council, "--state", "cancelled")
    warning_report = {"room": {"id": shared, "participants": [dict(room.participant(
        room.status(project, shared), room_owner))]}, "attempts": {"recent": [], "active_recent": []},
        "plan": {"tasks": [{"id": "review-%s" % n, "state": "review",
                             "claimed_by_participant": room_owner} for n in range(9)]},
        "review_backlog": {"threshold": 8, "counts": {room_owner: 9}}}
    control_write_command = [sys.executable, "-c", "pass", "--repo", str(project),
                             "--verify", "true", "--task-id", "backlog-warning"]
    from contextlib import redirect_stderr
    warning_output = io.StringIO()
    with patch.dict(os.environ, control_env, clear=True), \
            patch.object(panel, "provider_catalog", return_value=[control_codex, control_claude]), \
            patch.object(panel.shutil, "which", return_value="/fixture/provider"), \
            patch.object(panel, "routed_command", return_value=control_write_command), \
            patch.object(panel, "snapshot", return_value=(warning_report, 0)), \
            patch.object(panel.subprocess, "Popen", side_effect=observe_control_launch), \
            redirect_stderr(warning_output):
        warning_status = panel.dispatch(project, "claude", "worker", "light", "write", "implement",
                                        brief=brief, verify="true", task_id="backlog-warning",
                                        label="Backlog warning")
    assert warning_status == 0, (warning_status, warning_output.getvalue(), control_launch)
    assert warning_output.getvalue().splitlines() == [
        "plan backlog-warning has no recorded task; panel left it unchanged; plan protection is absent",
        "9 results await review/admission; review first"], warning_output.getvalue()
    warning_output = io.StringIO()
    with patch.dict(os.environ, control_env, clear=True), \
            patch.object(panel, "provider_catalog", return_value=[control_codex, control_claude]), \
            patch.object(panel.shutil, "which", return_value="/fixture/provider"), \
            patch.object(panel, "routed_command", return_value=control_write_command), \
            patch.object(panel, "snapshot", return_value=(dict(warning_report, review_backlog={
                "threshold": 9, "counts": {room_owner: 9}}), 0)), \
            patch.object(panel.subprocess, "Popen", side_effect=observe_control_launch), \
            redirect_stderr(warning_output):
        assert panel.dispatch(project, "claude", "worker", "light", "write", "implement",
                              brief=os.path.relpath(brief, project), verify="true", task_id="backlog-at-threshold",
                              label="Backlog at threshold") == 0
    assert warning_output.getvalue().splitlines() == [
        "plan backlog-at-threshold has no recorded task; panel left it unchanged; plan protection is absent"], \
        warning_output.getvalue()
    for owner in ("codex",):
        try:
            panel.dispatch(project, owner, "worker", "light", "read", "investigate", prompt="Inspect source")
        except ValueError as error:
            assert "unique active main" in str(error)
        else:
            raise AssertionError("control dispatch chose an exited owner")
    original_shared, original_room_owner = shared, room_owner
    shared = room.create(project, "legacy-multiple-main", "Legacy concurrent main refusal")
    room_owner = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main",
                              "--ref", "panel_role=main", "--ref", "panel_room_id=" + shared,
                              "--then", "starting", "--then", "working", output=True)
    legacy_main_join(project, shared, room_owner, "claude")
    os.environ["OMS_ROOM_ID"] = shared
    control_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project))
    second_owner = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                                "--ref", "panel_room_id=" + shared, "--then", "starting", "--then", "working", output=True)
    legacy_main_join(project, shared, second_owner, "claude")
    live_main_refusal = ("this room has 2 live mains; the panel supports one - release the others with "
                         "oms panel --release-main or finish the swap")
    try:
        panel.dispatch(project, "claude", "worker", "light", "read", "investigate", prompt="Inspect source")
    except ValueError as error:
        assert str(error) == live_main_refusal, error
    else:
        raise AssertionError("control dispatch silently chose between two mains")
    # A main whose shutdown is unconfirmed (blocked, not terminal) still owns its room.
    blocked_room = room.create(project, "legacy-blocked-main", "Unconfirmed shutdown still owns the room")
    working_main = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                                "--ref", "panel_room_id=" + blocked_room, "--then", "starting", "--then", "working", output=True)
    legacy_main_join(project, blocked_room, working_main, "claude")
    stuck_main = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                              "--ref", "panel_room_id=" + blocked_room, "--then", "starting", "--then", "working", output=True)
    legacy_main_join(project, blocked_room, stuck_main, "claude")
    panel.events(project, "transition", "--attempt", stuck_main, "--state", "blocked",
                 "--reason-code", "native_shutdown_unconfirmed")
    with patch.dict(os.environ, {"OMS_ROOM_ID": blocked_room, "OMS_ROOM_PARTICIPANT": working_main,
                                 "OMS_PANEL_MAIN_ATTEMPT": working_main}):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None, prompt="Blocked owner still live")
        except ValueError as error:
            assert str(error) == live_main_refusal, error
        else:
            raise AssertionError("an unconfirmed-shutdown main was ignored by the one-main guard")
    # A joined main with no lifecycle record at all is not proven gone either.
    unrecorded_room = room.create(project, "legacy-unrecorded-main", "A joined main without lifecycle history")
    recorded_main = panel.events(project, "start", "--provider", "claude", "--tool", "panel-main", "--ref", "panel_role=main",
                                 "--ref", "panel_room_id=" + unrecorded_room, "--then", "starting", "--then", "working", output=True)
    legacy_main_join(project, unrecorded_room, recorded_main, "claude")
    legacy_main_join(project, unrecorded_room, "att_" + "0" * 32, "claude")
    with patch.dict(os.environ, {"OMS_ROOM_ID": unrecorded_room, "OMS_ROOM_PARTICIPANT": recorded_main,
                                 "OMS_PANEL_MAIN_ATTEMPT": recorded_main}):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None, prompt="Unrecorded owner still joined")
        except ValueError as error:
            assert str(error) == live_main_refusal, error
        else:
            raise AssertionError("a joined main without lifecycle history was ignored by the one-main guard")
    # Neither --main nor a bound caller can select one of two live mains.
    for dry in (False, True):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None,
                           prompt="Ambiguous advice", main=second_owner, dry_run=dry)
        except ValueError as error:
            assert str(error) == live_main_refusal, error
        else:
            raise AssertionError("--main selected a main while two mains were live")
    for binding in ({"OMS_ROOM_PARTICIPANT": second_owner}, {"OMS_PANEL_MAIN_ATTEMPT": second_owner},
                    {"OMS_ROOM_PARTICIPANT": second_owner, "OMS_PANEL_MAIN_ATTEMPT": second_owner}):
        with patch.dict(os.environ, binding):
            try:
                panel.dispatch(project, "claude", "advisor", None, "read", None,
                               prompt="Bound caller cannot select a main", main=second_owner)
            except ValueError as error:
                assert str(error) == live_main_refusal, error
            else:
                raise AssertionError("bound caller dispatched while two mains were live")
            try:
                panel.owning_attempt(project, "claude", None, os.environ)
            except ValueError as error:
                assert str(error) == live_main_refusal, error
            else:
                raise AssertionError("bound caller continued while two mains were live")
    with patch.dict(os.environ, control_env, clear=True):
        try:
            panel.council(project, "claude", "legacy-council", "Legacy council", "Review the task")
        except ValueError as error:
            assert str(error) == live_main_refusal, error
        else:
            raise AssertionError("council started while two mains were live")
    with patch.dict(os.environ, dict(control_env, OMS_ROOM_PARTICIPANT=second_owner,
                                     OMS_PANEL_MAIN_ATTEMPT=second_owner), clear=True):
        try:
            panel.council(project, "claude", "legacy-bound-council", "Legacy council", "Review the task")
        except ValueError as error:
            assert str(error) == live_main_refusal, error
        else:
            raise AssertionError("bound council started while two mains were live")

    # Retire the original owner and preserve all identity-binding coverage with
    # the second main as the sole live owner.
    panel.events(project, "transition", "--attempt", room_owner, "--state", "cancelled")
    room.append(project, shared, {"kind": "leave", "participant": room_owner}, "Fixture owner stopped")
    for binding in ({"OMS_ROOM_PARTICIPANT": second_owner}, {"OMS_PANEL_MAIN_ATTEMPT": second_owner},
                    {"OMS_ROOM_PARTICIPANT": second_owner, "OMS_PANEL_MAIN_ATTEMPT": second_owner}):
        with patch.dict(os.environ, binding):
            before_preview = room.records(project, shared)
            assert panel.dispatch(project, "claude", "advisor", None, "read", None,
                                  prompt="Preview the second main", main=second_owner, dry_run=True) == 0
            assert room.records(project, shared) == before_preview
            assert panel.dispatch(project, "claude", "advisor", None, "read", None,
                                  prompt="Advise the second main", main=second_owner) == 0
        assert room.status(project, shared)["participants"][-1]["parent"] == second_owner
        assert control_launch["env"]["OMS_PANEL_MAIN_ATTEMPT"] == second_owner
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner,
                                "OMS_PANEL_MAIN_ATTEMPT": second_owner}):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None, prompt="Conflicting caller")
        except ValueError as error:
            assert "identities disagree" in str(error), error
        else:
            raise AssertionError("dispatch accepted conflicting caller identities")
    try:
        panel.dispatch(project, "claude", "advisor", None, "read", None,
                       prompt="Advise nobody", main="outside-main")
    except ValueError as error:
        assert "different main than this caller" in str(error), error
    else:
        raise AssertionError("explicit main bypassed live-main proof")
    # An inherited caller cannot be redirected to another main by --main.
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": second_owner}):
        try:
            panel.dispatch(project, "claude", "advisor", None, "read", None,
                           prompt="Misattributed advice", main=room_owner)
        except ValueError as error:
            assert "different main" in str(error)
        else:
            raise AssertionError("--main silently kept the inherited caller")
    # A main moved to Claude Code's background service keeps only its session id;
    # its room binding names the caller, with or without a selected room.
    moved_sid = "3f0c2a4e-9b1d-4c6e-8a7f-1d2e3c4b5a69"
    room.bind(project, shared, second_owner, moved_sid)
    assert not room.discover(project)[1]
    for selected in (shared, None):
        with patch.dict(os.environ, {"CLAUDE_CODE_SESSION_ID": moved_sid}):
            if not selected:
                os.environ.pop("OMS_ROOM_ID")
            assert panel.dispatch(project, "claude", "advisor", None, "read", None,
                                  prompt="Advise the moved main") == 0
        assert room.status(project, shared)["participants"][-1]["parent"] == second_owner
        assert control_launch["env"]["OMS_PANEL_MAIN_ATTEMPT"] == second_owner
    worker_participant = next(p["participant"] for p in room.status(project, shared)["participants"]
                              if p.get("parent") == second_owner)
    for refused in ({"CLAUDE_CODE_SESSION_ID": "0d9e8f7a-6b5c-4d3e-2f1a-0b9c8d7e6f5a",
                     "OMS_ROOM_PARTICIPANT": "unbound-session"},
                    {"CLAUDE_CODE_SESSION_ID": moved_sid, "OMS_HARNESS_CHILD": "1",
                     "OMS_ROOM_PARTICIPANT": worker_participant}):
        with patch.dict(os.environ, refused):
            try:
                panel.dispatch(project, "claude", "advisor", None, "read", None, prompt="Advise nobody")
            except ValueError as error:
                assert "unique active main" in str(error)
            else:
                raise AssertionError("dispatch resolved an unbound or worker session to a main")
    # Calls have their own cap: a room whose member slots are full still admits calls.
    cap_rows = room.records(project, shared)
    members_now = sum(not p.get("parent") for p in room.status(project, shared)["participants"])
    calls_now = sum(bool(p.get("parent")) for p in room.status(project, shared)["participants"])
    call_join = {"kind": "join", "participant": "call-capacity", "provider": "codex", "role": "advisor",
                 "model": "gpt-6-astra", "label": "Capacity", "owns": [], "parent": second_owner}
    with patch.object(room, "MAX_PARTICIPANTS", members_now):
        room.validate_event(cap_rows, call_join, "Joined: Capacity")
        try:
            room.validate_event(cap_rows, dict(call_join, participant="main-capacity", role="main", parent=None),
                                "Joined: Capacity")
        except ValueError as error:
            assert "participant limit" in str(error)
        else:
            raise AssertionError("room member cap was not enforced")
    with patch.object(room, "MAX_CALLS", calls_now):
        try:
            room.validate_event(cap_rows, call_join, "Joined: Capacity")
        except ValueError as error:
            assert "call limit" in str(error)
        else:
            raise AssertionError("room call cap was not enforced")
    panel.events(project, "transition", "--attempt", second_owner, "--state", "cancelled")
    room.append(project, shared, {"kind": "leave", "participant": second_owner}, "Fixture owner stopped")
    shared, room_owner = original_shared, original_room_owner
    os.environ["OMS_ROOM_ID"] = shared
    control_env = dict(environment, OMS_ROOM_ID=shared, OMS_ROOM_REPO=str(project))

_operator_block("room-scopes-and-admission")
recorded = json.loads(call(["bash", str(panel.ENTRY), "agent-events", "--repo", str(project), "list", "--limit", "300", "--json"]))
room_attempt = next(a for a in recorded if a.get("refs", {}).get("panel_room_participant") == child["participant"])
assert room_attempt["refs"]["panel_room_id"] == shared
observed = next(row for row in map(json.loads, reversed(worker_log.read_text().splitlines()))
                if row["participant"] == child["participant"])
assert observed["room"] == shared, observed
# Declared scopes: only the participant replaces or clears its own; overlap is a path-component prefix.
assert room.overlaps("scripts/lib", "scripts/lib/room.py") and room.overlaps("scripts/lib/room.py", "scripts/lib")
assert room.overlaps("docs", "docs") and room.overlaps(".", "docs") and not room.overlaps("scripts/li", "scripts/lib")
other_main = main_attempts["codex"]
room.join(project, shared, "scope-advisor", "codex", role="advisor", parent=room_owner)
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, ["docs", "scripts/lib"])
    assert room.participant(room.status(project, shared), room_owner)["owns"] == ["docs", "scripts/lib"]
    room.scope(project, shared, room_owner, [])
    assert room.participant(room.status(project, shared), room_owner)["owns"] == []
for who, scopes, reason in ((room_owner, ["s%d" % n for n in range(17)], "sixteen"), (room_owner, ["./docs"], "normalized"),
                            ("scope-advisor", ["docs"], "advisors and reviewers"), (other_main, ["docs"], "its own scope")):
    with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner if reason == "its own scope" else who}):
        try:
            room.scope(project, shared, who, scopes)
        except ValueError as error:
            assert reason in str(error), (reason, error)
        else:
            raise AssertionError("scope was not refused: " + reason)
scope_cli = subprocess.run(["bash", str(panel.ENTRY), "room", "scope", "--repo", str(project), "--id", shared,
                            "--participant", other_main, "--owns", "scripts/lib/room.py"],
                           env=dict(environment, OMS_ROOM_PARTICIPANT=other_main), capture_output=True, text=True, timeout=30)
assert scope_cli.returncode == 0 and json.loads(scope_cli.stdout)["owns"] == ["scripts/lib/room.py"], scope_cli
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, ["scripts/lib"])
room.join(project, shared, "scope-live", "codex", role="worker", parent=room_owner, owns=["scripts"])
room.join(project, shared, "scope-done", "codex", role="worker", parent=room_owner, owns=["scripts/lib/panel_view.py"])
room.send(project, shared, "scope-done", room_owner, "Call exit=0; parent acceptance pending.", kind="handoff",
          message_id="result-scope-done")
panel.events(project, "start", "--provider", "codex", "--tool", "peer-delegate",
             "--ref", "panel_role=worker", "--ref", "panel_room_id=" + shared,
             "--ref", "panel_room_participant=scope-done", "--then", "starting", "--then", "working",
             "--then", "verifying", "--then", "review", "--then", "done")
scope_before_preview = room.records(project, shared)
scoped_dispatch = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                  "--dispatch", "worker", "--owner", "claude", "--access", "write", "--purpose", "implement",
                                  "--brief-file", str(brief), "--verify", "test -f value.txt", "--scope", "./scripts/lib/",
                                  "--scope", "docs", "--dry-run"], env=room_env, capture_output=True, text=True, timeout=60)
assert scoped_dispatch.returncode == 0, scoped_dispatch
scoped_plan = json.loads(scoped_dispatch.stdout)
assert sorted((o["participant"], o["scope"]) for o in scoped_plan["overlaps"]) == sorted(
    [(other_main, "scripts/lib/room.py"), ("scope-live", "scripts"),
     ("scope-done", "scripts/lib/panel_view.py")]), scoped_plan
assert scoped_dispatch.stderr.count("Scope overlap") == 1 and other_main in scoped_dispatch.stderr, scoped_dispatch.stderr
assert room.records(project, shared) == scope_before_preview, "scope preview changed the room"
unscoped = json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared, "--dispatch", "worker",
                            "--owner", "claude", "--access", "write", "--purpose", "implement", "--brief-file", str(brief),
                            "--verify", "test -f value.txt", "--dry-run"], room_env))
assert "overlaps" not in unscoped, unscoped
fake_bin = temporary / "dispatch-bin"
fake_bin.mkdir()
provider_marker = temporary / "provider-invoked"
fake_provider = fake_bin / "claude"
fake_provider.write_text("#!/bin/sh\nprintf called > \"$OMS_FAKE_PROVIDER_MARKER\"\n")
fake_provider.chmod(0o700)
dry_env = dict(room_env, PATH=str(fake_bin) + os.pathsep + os.environ.get("PATH", ""),
               OMS_FAKE_PROVIDER_MARKER=str(provider_marker))
clean_prompt = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                               "--dispatch", "worker", "--owner", "claude", "--access", "read",
                               "--purpose", "investigate", "--prompt", "Inspect value.txt.", "--dry-run"],
                              env=dry_env, capture_output=True, text=True, timeout=60)
assert clean_prompt.returncode == 0 and json.loads(clean_prompt.stdout)["executes"] is False, clean_prompt
room_before_scan = room.status(project, shared)["participants"]
sensitive_prompt = "Inspect source.\n" + sensitive_summary
blocked_prompt = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                 "--dispatch", "worker", "--owner", "claude", "--access", "read",
                                 "--purpose", "investigate", "--prompt", sensitive_prompt, "--dry-run"],
                                env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_prompt.returncode != 0 and not blocked_prompt.stdout, blocked_prompt
assert re.search(r"secret-tier match in (prompt|brief file) at line 2:token-assignment", blocked_prompt.stderr), blocked_prompt.stderr
assert "fixture-value" not in blocked_prompt.stderr and room.status(project, shared)["participants"] == room_before_scan
sensitive_brief = temporary / "sensitive-brief.md"
sensitive_brief.write_text("Inspect source.\npass" + "word=fixture-value")
blocked_brief = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                "--dispatch", "worker", "--owner", "claude", "--access", "write",
                                "--purpose", "implement", "--brief-file", str(sensitive_brief),
                                "--verify", "test -f value.txt", "--dry-run"],
                               env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_brief.returncode != 0 and not blocked_brief.stdout, blocked_brief
assert re.search(r"secret-tier match in (prompt|brief file) at line 2:token-assignment", blocked_brief.stderr), blocked_brief.stderr
assert "fixture-value" not in blocked_brief.stderr and room.status(project, shared)["participants"] == room_before_scan
blocked_scoped = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                                 "--dispatch", "worker", "--owner", "claude", "--access", "write",
                                 "--purpose", "implement", "--brief-file", str(sensitive_brief),
                                 "--verify", "test -f value.txt", "--scope", "docs", "--dry-run"],
                                env=dry_env, capture_output=True, text=True, timeout=60)
assert blocked_scoped.returncode != 0 and not blocked_scoped.stdout, blocked_scoped
assert re.search(r"secret-tier match in (prompt|brief file) at line 2:token-assignment", blocked_scoped.stderr), blocked_scoped.stderr
assert "fixture-value" not in blocked_scoped.stderr and room.status(project, shared)["participants"] == room_before_scan
for name, prepare in (
        ("symlink", lambda path: path.symlink_to(sensitive_brief)),
        ("missing", lambda path: None),
        ("unreadable", lambda path: (path.write_text("bounded"), path.chmod(0))),
        ("oversized", lambda path: path.write_bytes(b"x" * (panel.MAX_PROMPT + 1)))):
    invalid_brief = temporary / (name + "-brief.md")
    prepare(invalid_brief)
    refused = subprocess.run(["bash", str(panel.ENTRY), "panel", "--repo", str(project), "--room", shared,
                             "--dispatch", "worker", "--owner", "claude", "--access", "write",
                             "--purpose", "implement", "--brief-file", str(invalid_brief),
                             "--verify", "test -f value.txt", "--dry-run"],
                            env=dry_env, capture_output=True, text=True, timeout=60)
    assert refused.returncode != 0 and not refused.stdout, (name, refused)
    assert "brief must be a readable regular file" in refused.stderr, (name, refused.stderr)
    assert str(invalid_brief) not in refused.stderr
    assert room.status(project, shared)["participants"] == room_before_scan
    if name == "unreadable":
        invalid_brief.chmod(0o600)
assert not provider_marker.exists(), "dry-run invoked the fake provider"
call(plan_cli + ["add", "--id", "preflight-refusal", "--title", "Refused before worker start"])
refusal_task = call(plan_cli + ["show", "--id", "preflight-refusal"])
workers_before_refusal = worker_log.read_text()
refused_write = subprocess.run(blocked_brief.args[:-1] + ["--task-id", "preflight-refusal"],
                               env=room_env, capture_output=True, text=True, timeout=90)
assert refused_write.returncode != 0 and "preflight refused before task claim" in refused_write.stderr, refused_write
assert call(plan_cli + ["show", "--id", "preflight-refusal"]) == refusal_task
assert room.status(project, shared)["participants"] == room_before_scan
assert worker_log.read_text() == workers_before_refusal, "preflight refusal invoked a provider"
shutil.rmtree(project / ".oms/plan")  # later board fixtures expect a project with no plan
scoped_run = subprocess.run(scoped_dispatch.args[:-1], env=dict(room_env, PANEL_TEST_MODE="write"),
                            capture_output=True, text=True, timeout=90)
assert scoped_run.returncode == 0 and "Scope overlap" in scoped_run.stderr, scoped_run
# Its patch now gets its own automatic reviewer even when an earlier call returned identical bytes.
scoped_call = [p for p in room.status(project, shared)["participants"]
               if p["participant"].startswith("call-") and p["role"] != "reviewer"][-1]
assert scoped_call["owns"] == ["scripts/lib", "docs"] and scoped_call["parent"] == room_owner, scoped_call
room.append(project, shared, {"kind": "leave", "participant": scoped_call["participant"]}, "Left")
with patch.dict(os.environ, {"OMS_ROOM_PARTICIPANT": room_owner}):
    room.scope(project, shared, room_owner, [])
for who in ("scope-advisor", "scope-live", "scope-done"):
    room.append(project, shared, {"kind": "leave", "participant": who}, "Left")
import hook_state
with patch.dict(os.environ, {"OMS_PANEL_MAIN_ATTEMPT": room_owner, "OMS_PANEL_SESSION": "oms-fixture"}):
    tower = hook_state.panel_main_hint({"hook_event_name": "UserPromptSubmit"})
    assert "oms room scope" in tower
# Same task reuse cannot return another participant's artifact as the result.
assert not panel.results(project, "room-inspection", "unrelated-participant")["rows"][0]["calls"]
participant_results = panel.results(project, room_participant=child["participant"])
assert any("Answer:" in c.get("answer", "") for r in participant_results["rows"] for c in r["calls"]), participant_results
standalone_env = {key: value for key, value in environment.items()
                  if key not in {"OMS_ROOM_ID", "OMS_ROOM_PARTICIPANT", "OMS_PANEL_MAIN_ATTEMPT"}}
with patch.dict(os.environ, standalone_env, clear=True), \
        patch.object(panel, "routed_command", return_value=[sys.executable, "-c", "pass", "--repo", str(project)]), \
        patch("sys.stderr", new=io.StringIO()) as standalone_notice:
    assert panel.dispatch(project, "codex", "worker", "light", "read", "investigate",
                          prompt="Inspect source", task_id="standalone-fixture") == 0
    assert "room and main relationship are unrecorded" in standalone_notice.getvalue()
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "inline", "--dry-run"]))["host"] == "inline"
assert json.loads(call(["bash", str(panel.ENTRY), "panel", "--repo", str(project),
    "--host", "herdr", "--dry-run"]))["host"] == "herdr"

# End simulated owners before each real native-start phase; no fixture CLI remains running here.
