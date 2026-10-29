execution_repo = temporary / "verification-execution-root"
execution_repo.mkdir()
import panel_dispatch as dispatch
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch
import io
from concurrent.futures import ThreadPoolExecutor

# Exact tier models keep main ownership and judgment on the native pair.
def exercise_exact_model_routing():
    from panel_routing import valid_route
    import panel_routing
    load_policy = panel_routing.policy
    original = load_policy()
    assert original["tiers"]["light"][0] == {"provider": "pi", "model": "openrouter/thinkingmachines/inkling:free", "effort": "low", "public_only": True}
    for owner in ("claude", "codex"):
        installed = {"pi", "claude", "codex"}
        assert allocate(owner, role="documenter", installed=installed)["provider"] == "pi"
        assert allocate(owner, role="researcher", installed=installed)["provider"] in {"claude", "codex"}
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
                        patch.object(panel, "run_dispatch", side_effect=lambda *args: args[-2]):
                    fallback = panel.dispatch(project, "codex", "worker", None, "read", "explain", prompt="Inspect source.")
                    assert (fallback["provider"] if fallback else None) == expected, fallback
            # The board and discovery keep handling a third tier provider without promoting a main.
            text = render({"repo": {"name": "routing"}, "collection": {"ok": True},
                           "room": {"participants": [dict(participant="oc", provider=provider, model=model,
                                                             role="worker", joined=True)]}}, "codex", width=150, height=40)
            assert isinstance(text, str)

with patch.dict(os.environ, {"OMS_PANEL_PUBLIC_ROUTE_OPT_IN": "1"}):
    exercise_exact_model_routing()

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
            patch.object(panel, "run_dispatch", side_effect=lambda *args: args[-2]):
        fallback = panel.dispatch(state_repo, "codex", "worker", None, "read", "explain",
                                  prompt="Inspect source.", work_repo=work_repo)
        assert fallback is None, fallback
        assert alternate.call_args.kwargs["repo"] == state_repo

exercise_public_only_routing()

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

def run_continuation_fixture(room_repo, work_repo, base):
    import panel_results as continuation_results
    call = {"attempt_id": "worker-attempt", "exit": 0, "patch": "patches/continued.patch",
            "patch_sha256": "fixture-sha", "event_id": "fixture-event"}
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
with patch.object(panel, "selected_route", return_value=warning_route), \
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
with patch.object(panel, "selected_route", return_value=warning_route), \
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
            patch.object(panel, "routing_status", return_value=readings) as status, \
            patch.object(panel, "run_dispatch", side_effect=lambda *args: args[-2]):
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
    planned = json.loads(call(request + ["--dry-run"], pi_env))
    assert planned["route"]["provider"] == "pi" and "--reasoning-effort" in planned["argv"]
    answer = call(request, dict(pi_env, PANEL_TEST_MODE="write",
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
        "claude:model=claude-opus-5-5,codex:model=gpt-6.1-sol,claude:model=claude-sonnet-5-5,codex:model=gpt-6.1-terra"
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

for has_write_patch, expected in (
        (True, "after oms land run oms agent-plan finish --id hint-task --landed-commit SHA"),
        (False, "a task with no patch cannot finish by landing; close it with oms agent-plan accept-main-research (completion bundle) or oms agent-plan block --id hint-task --reason TEXT")):
    hint = io.StringIO()
    with patch.object(saved.subprocess, "run", return_value=SimpleNamespace(
            returncode=0, stdout='{"state":"review"}', stderr="")), redirect_stderr(hint):
        saved._plan_hint(project, "hint-task", has_write_patch)
    assert expected in hint.getvalue(), hint.getvalue()
    if not has_write_patch:
        assert "after oms land" not in hint.getvalue(), hint.getvalue()
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
# A live canonical review call defers finalize before its result artifact is written.
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
malformed_review.mkdir()
deferred_args = list(final_args)
deferred_args[deferred_args.index("--task-id") + 1] = "finalize-review-read-pass"
deferred_args[deferred_args.index("--outcome") + 1] = "completed"
try:
    deferred = panel_with_review_wait(deferred_args[3:], 2, cwd=project, env=environment)
    assert deferred.returncode == 75 and \
        "review-read-pass-fixture" in deferred.stderr and "review-malformed-fixture" in deferred.stderr, deferred.stderr
    assert deferred.stderr.count("waiting up to") == 1, deferred.stderr
    assert not any(row.get("kind") == "panel-result" and row.get("task_id") == "finalize-review-read-pass"
                   for row in saved._records(project)[1]), "deferred finalize wrote a result"
finally:
    review_owner.communicate("release\n", timeout=10)
    shutil.rmtree(malformed_review)
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
        subprocess.run(["bash", "-c", recovery_commands[0]], env=failure_env, check=True)
        assert json.loads(accept_call("agent-plan", "show", "--id", "accept-task", execution=failure_repo,
                                      env=failure_env).stdout)["state"] == "review"
accepted = accept_call("panel", *accept_options, execution=accept_execution)
assert "committed locally; publication pending" in accepted.stdout and "Next: oms land --repo" in accepted.stdout
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
failed = accept_call("panel", *accept_finalize, execution=accept_execution, check=False)
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
# BEGIN patch-scope finalize regression (also runnable as an isolated case).
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
                    assert view["representative_state"] == "main_decision accepted" and view["calls"][0]["superseded"], view
            settled_retry = review_settlement._settle(project, [previous], [], {"scope-main"},
                {"scope-result": (accepted_retry, {"ts": "2026-10-17T00:00:00Z"})})
            assert not settled_retry["pending"] and settled_retry["superseded"][0]["attempt"] == previous["attempt_id"]
    previous = dict(failed_worker, state="review")
    continued = dict(retry_worker, refs=dict(retry_worker["refs"], panel_continues=previous["attempt_id"]))
    with patch.object(saved, "_records", return_value=([previous, continued], [failed_row] + retry_rows)):
        result = saved.finalize(project, "codex", "scope-result", str(summary), "accepted", "true", ["value.txt"])
        assert result["superseded_attempts"] == [{"attempt_id": previous["attempt_id"],
            "superseded_by": continued["attempt_id"], "reason": "continued"}], result
    middle = dict(continued, attempt_id="att_middle", created_at="2026-10-16T00:00:01.5Z")
    last = dict(continued, refs=dict(continued["refs"], panel_continues=middle["attempt_id"]))
    selected, landed, superseded = saved._write_candidates([previous, middle, last], [None, None, {"event_id": "landed"}], [], "codex")
    assert selected == [last] and len(superseded) == 2 and all(item["reason"] == "continued" for item in superseded)
    for ref in (None, "", "malformed", "att_missing"):
        candidate = dict(continued, refs=dict(continued["refs"], panel_continues=ref))
        selected, _, superseded = saved._write_candidates([previous, candidate], [None, {"event_id": "landed"}], [], "codex")
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
         "timed out", "timed out", "continue with --continue TASK_ID"),
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
            assert "panel evidence must be a regular non-symlink, non-hard-linked file" in error_text.getvalue(), error_text.getvalue()
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
