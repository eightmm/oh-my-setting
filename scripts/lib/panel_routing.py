"""Role allocation for panel-owned tasks; authority is independent of workload."""

import json
import os
import sys
import tempfile
import time
from datetime import datetime, timezone
import re
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MAIN_OWNERS = ("claude", "codex", "pi", "antigravity")


def main_owner_matches(harness, owner):
    """Match durable pairs; callers still prove adapter registration and lifecycle ownership."""
    if not isinstance(harness, str) or not isinstance(owner, str) or not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", harness):
        return False
    pairs = {"claude": ("claude",), "codex": ("codex",), "pi": ("codex", "pi"), "agy": ("antigravity",)}
    return owner in pairs.get(harness, (harness,))


MODEL_IDS = {"codex": {"gpt-6.1-sol", "gpt-6-sol", "gpt-6.1-terra", "gpt-6-luna", "gpt-6-astra"},
             "claude": {"claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-haiku-5-5"}}
EXACT_MODEL_PROVIDERS = {"opencode", "pi"}
TIERS = ("light", "routine", "deep", "frontier")
# The 5.x Codex generation is never used (user decision 2026-10-27).
CODEX_MIN_MAJOR = 6
# Role strategy -> the board kind recorded as panel_role; acceptance, scopes and continuation key on the kind.
KINDS = {"worker": "worker", "builder": "worker", "refactorer": "worker", "documenter": "worker",
         "integrator": "worker", "performance-engineer": "worker", "release-engineer": "worker",
         "researcher": "researcher", "advisor": "advisor", "architect": "advisor", "critic": "advisor",
         "reviewer": "reviewer", "verifier": "reviewer", "security-auditor": "reviewer"}
WRITERS = {"worker", "builder", "refactorer", "documenter", "integrator", "performance-engineer"}
# Judges of another context's work prefer the provider other than the main's when the tier offers it.
INDEPENDENT = {"reviewer", "verifier", "security-auditor", "critic"}
RESEARCH_FAMILIES = {"luna", "haiku"}
# Weekly readings are ratios of unlike limits: within `gap` points count as equal.
TIE_BAND_DEFAULTS = {"gap": 15}


def route_family(provider, model):
    """(family, version) of a versioned role model; scripts/lib/model-capability.sh oms_codex_route_family is the shell twin."""
    pattern = {"codex": r"gpt-(\d+(?:\.\d+)?)-(astra|sol|terra|luna)",
               "claude": r"claude-(opus|sonnet|fable|haiku)-(\d+(?:-\d+)*)"}.get(provider)
    match = re.fullmatch(pattern, model) if pattern and isinstance(model, str) else None
    if not match:
        return None
    number, family = match.groups() if provider == "codex" else reversed(match.groups())
    version = tuple(int(part) for part in re.split(r"[.-]", number))
    while len(version) > 1 and version[-1] == 0:
        version = version[:-1]
    return family, version


def newest_route_model(provider, configured, catalog):
    known = route_family(provider, configured)
    if not known:
        raise ValueError("role routes require a versioned model family")
    candidates = {configured, *(m for m in catalog if route_family(provider, m)
                               and route_family(provider, m)[0] == known[0])}
    return max(candidates, key=lambda model: (route_family(provider, model)[1], model == configured, model))


def routable_route_model(provider, configured, catalog):
    """Resolve native families or exact provider IDs; an empty exact catalog defers to the CLI.

    A Codex family behind the catalog's newest major generation is refused, matching the shell's routable set.
    """
    if provider in EXACT_MODEL_PROVIDERS:
        if catalog and configured not in catalog:
            raise ValueError("%s exact model %s is absent from the catalog" % (provider, configured))
        return configured
    model = newest_route_model(provider, configured, catalog)
    if provider == "codex":
        majors = [family[1][0] for family in (route_family(provider, m) for m in catalog) if family]
        if majors and route_family(provider, model)[1][0] < max(majors):
            raise ValueError("codex %s is a major generation behind gpt-%d in the catalog; the %s family is not routed"
                             % (model, max(majors), route_family(provider, model)[0]))
    return model


def valid_route(route):
    # A Codex route names any version of a known family; the catalog picks the newest one at dispatch.
    if (not isinstance(route, dict) or not {"provider", "model", "effort"} <= set(route) <= {"provider", "model", "effort", "public_only"}
            or ("public_only" in route and type(route["public_only"]) is not bool)
            or not all(isinstance(route[key], str) for key in ("provider", "model", "effort"))
            or route["effort"] not in {"low", "medium", "high"}):
        return False
    provider, model = route["provider"], route["model"]
    if provider in EXACT_MODEL_PROVIDERS:
        return re.fullmatch(r"[^\s/\x00-][^\s/\x00]*/[^\s\x00]+", model) is not None
    if provider == "codex":
        return route_family(provider, model) is not None
    return provider in MODEL_IDS and model in MODEL_IDS[provider]


def policy():
    value = json.loads((ROOT / "config/panel-routing.json").read_text(encoding="utf-8"))
    if (not isinstance(value, dict) or not {"schema", "main", "tiers", "roles"} <= set(value)
            <= {"schema", "main", "tiers", "roles", "tie_band", "auto_review"} or value.get("schema") != 2):
        raise ValueError("unsupported panel routing policy")
    if "auto_review" in value and type(value["auto_review"]) is not bool:
        raise ValueError("panel auto-review setting must be boolean")
    value.setdefault("auto_review", True)
    band = value.get("tie_band", {})
    if (not isinstance(band, dict) or not set(band) <= set(TIE_BAND_DEFAULTS)
            or any(type(number) is not int or not 0 <= number <= 100 for number in band.values())):
        raise ValueError("panel tie band limits must be integer percentages")
    value["tie_band"] = dict(TIE_BAND_DEFAULTS, **band)
    mains = value.get("main")
    if not isinstance(mains, dict) or set(mains) != set(MODEL_IDS):
        raise ValueError("invalid panel main routes")
    for key in ("codex", "claude"):
        if not valid_route(mains[key]) or mains[key]["provider"] != key:
            raise ValueError("invalid panel route: main/%s" % key)
    tiers = value.get("tiers") if isinstance(value.get("tiers"), dict) else {}
    if set(tiers) != set(TIERS):
        raise ValueError("invalid panel tiers")
    for tier in TIERS:
        if not isinstance(tiers.get(tier), list) or not tiers[tier] or not all(map(valid_route, tiers[tier])):
            raise ValueError("invalid panel tier: %s" % tier)
        if len({c["provider"] for c in tiers[tier]}) != len(tiers[tier]):
            raise ValueError("duplicate provider in panel tier: %s" % tier)
    roles = value.get("roles")
    if not isinstance(roles, dict) or set(roles) != set(KINDS):
        raise ValueError("panel roles must map every role to a tier")
    for role, route in roles.items():
        if (not isinstance(route, dict) or set(route) != {"tier"}
                or not isinstance(route.get("tier"), str) or route["tier"] not in TIERS):
            raise ValueError("invalid panel role: %s" % role)
    return value


def weekly_used(provider_status, provider):
    """The board's rounded weekly usage for a provider; None when stale, malformed or missing."""
    from panel_metrics import percent
    row = provider_status.get(provider) if isinstance(provider_status, dict) else None
    return percent(row.get("weekly_used")) if isinstance(row, dict) else None


def daily_capped(provider_status, candidate):
    row = provider_status.get(candidate["provider"], {}) if isinstance(provider_status, dict) else {}
    caps = row.get("daily_cap", {}) if isinstance(row, dict) else {}
    day = caps.get(candidate["model"]) if isinstance(caps, dict) else None
    return isinstance(day, str) and day == datetime.now(timezone.utc).strftime("%Y-%m-%d")


def repository_public(repo):
    """Verify origin visibility against the state/room repository; never the worker checkout."""
    if os.environ.get("OMS_PANEL_PUBLIC_ROUTE_OPT_IN") == "1":
        return True
    # Resolve the authoritative repository (state/room repo), never the work checkout.
    authoritative = repo or os.environ.get("OMS_ROOM_REPO") or os.getcwd()
    try:
        top = subprocess.run(["git", "-C", str(authoritative), "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True, timeout=5, check=True,
                             stdin=subprocess.DEVNULL).stdout.strip()
        authoritative = top or authoritative
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        pass
    if not authoritative or not isinstance(authoritative, (str, Path)):
        return False
    try:
        remote = subprocess.run(["git", "-C", str(authoritative), "remote", "get-url", "origin"],
                                capture_output=True, text=True, timeout=5, check=True,
                                stdin=subprocess.DEVNULL).stdout.strip()
        normalized = subprocess.run([sys.executable, str(ROOT / "scripts/lib/github-remote.py"), remote],
                                    capture_output=True, text=True, timeout=5, check=True,
                                    stdin=subprocess.DEVNULL).stdout.strip()
        cache = Path(authoritative) / ".oms/panel-state/public-route-visibility.json"
        # Only a not-public answer is cached (10 minutes); public is re-verified on every check, so a
        # repository made private never keeps free third-party routes.
        def _cache_not_public():
            try:
                cache.parent.mkdir(parents=True, exist_ok=True)
                fd, tmp = tempfile.mkstemp(prefix=".visibility-", dir=str(cache.parent))
                try:
                    with os.fdopen(fd, "w", encoding="utf-8") as stream:
                        json.dump({"origin": normalized, "checked_at": time.time(), "public": False}, stream)
                    os.replace(tmp, cache)
                finally:
                    if os.path.exists(tmp):
                        os.unlink(tmp)
            except OSError:
                pass
        try:
            data = json.loads(cache.read_text(encoding="utf-8"))
            age = time.time() - data["checked_at"]
            if data.get("origin") == normalized and data.get("public") is False and 0 <= age < 600:
                return False
        except (OSError, ValueError, TypeError, KeyError):
            pass
        try:
            result = subprocess.run(["gh", "repo", "view", "github.com/" + normalized, "--json", "visibility"],
                                    capture_output=True, text=True, timeout=5, check=True,
                                    stdin=subprocess.DEVNULL)
            visibility = json.loads(result.stdout)["visibility"]
            if visibility not in ("PUBLIC", "PRIVATE", "INTERNAL"):
                raise ValueError("unknown visibility: %s" % visibility)
            if visibility == "PUBLIC":
                return True
            _cache_not_public()
            return False
        except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError) as exc:
            # Blocked network or missing CLI: cache not-public.
            _cache_not_public()
            return False
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        return False


def public_candidates(candidates, repo):
    if any(c.get("public_only") for c in candidates) and not repository_public(repo):
        return [c for c in candidates if not c.get("public_only")]
    return candidates


def allocate(owner, role="worker", tier=None, access="read", purpose=None, installed=None, provider_status=None,
             recent=None, writer=None, catalogs=None, owner_model=None, repo=None, check_visibility=True):
    """Availability, quota left, then independent judgment; explicit targets narrow installed.

    recent maps provider -> dispatches in this room and writer is the provider that wrote the reviewed task.
    catalogs maps provider -> listed models; a candidate routable_route_model refuses there is ineligible."""
    if not owner or role not in KINDS:
        raise ValueError("choose a supported main and role")
    routes = policy()
    tier = tier or routes["roles"][role]["tier"]
    if tier not in TIERS or access not in ("read", "write"):
        raise ValueError("invalid workload or access")
    if access == "write" and role not in WRITERS:
        raise ValueError("%s is read-only" % role)
    if role == "worker":
        purpose = purpose or ("implement" if access == "write" else "explain")
        if purpose not in ("explain", "investigate", "implement") or (
                purpose == "implement" and access != "write" or purpose == "explain" and access != "read"):
            raise ValueError("worker purpose must match access: explain reads, investigate reads or writes, implement writes")
    elif purpose:
        raise ValueError("--purpose applies to plain workers; the role names the task")
    else:
        purpose = {"advisor": "advise", "reviewer": "review", "researcher": "research"}.get(role, role)
    candidates = routes["tiers"][tier]
    if role in INDEPENDENT or KINDS[role] == "advisor":
        candidates = [c for c in candidates if c["provider"] in MODEL_IDS]
    if role == "researcher":
        if tier != "light":
            raise ValueError("researchers use the light tier")
        candidates = [c for c in candidates if route_family(c["provider"], c["model"])
                      and route_family(c["provider"], c["model"])[0] in RESEARCH_FAMILIES]
    usable = [c for c in candidates if installed is None or c["provider"] in installed]
    if check_visibility:
        usable = public_candidates(usable, repo)
    if not usable:
        raise ValueError("no installed provider serves the %s tier (%s); install one or use --to" % (
            tier, ", ".join(sorted({c["provider"] for c in candidates})) or "none configured"))
    usable = [c for c in usable if not daily_capped(provider_status, c)]
    if not usable:
        raise ValueError("all installed providers in the %s tier are in daily cooldown" % tier)
    # Quota guards any choice among installed providers, including the one family exclusion leaves.
    several = len(usable) > 1
    if catalogs is not None:
        eligible, refused = [], None
        for candidate in usable:
            try:
                routable_route_model(candidate["provider"], candidate["model"], catalogs.get(candidate["provider"], []))
                eligible.append(candidate)
            except ValueError as error:
                refused = error
        if not eligible:
            raise refused
        usable = eligible
    if several:
        # Use the board's rounded reading; stale, malformed and missing readings remain available.
        available, used_by = [], {}
        for candidate in usable:
            used = weekly_used(provider_status, candidate["provider"])
            if used is None or used < 90:
                available.append(candidate)
                used_by[candidate["provider"]] = used
        if not available:
            raise ValueError("all installed providers in the %s tier have <=10%% weekly quota left" % tier)
        # With a reading for every remaining provider, the one with more weekly quota left goes first,
        # unless the readings sit in the tie band: then fewer recent dispatches, else away from the main.
        # Without a full set of readings, spread load away from the provider the main itself consumes.
        if None not in used_by.values():
            band = routes["tie_band"]
            tied = max(used_by.values()) - min(used_by.values()) < band["gap"]
            if tied:
                available = sorted(available, key=lambda c: ((recent or {}).get(c["provider"], 0),
                                                              c["provider"] == owner))
            else:
                available = sorted(available, key=lambda c: used_by[c["provider"]])
        else:
            available = sorted(available, key=lambda c: c["provider"] == owner)
        usable = available
    # Read-only investigate workers prefer non-Pi; apply after catalog/quota ordering.
    if role == "worker" and access == "read" and purpose == "investigate":
        non_pi = [c for c in usable if c["provider"] != "pi"]
        if non_pi:
            usable = non_pi
        # else: keep list as is (degraded beats refused)
    if role in INDEPENDENT:
        # An exact-model writer (Pi, OpenCode) is never a native judge, so ordering away from it keeps tier order;
        # a Pi main judging its own provider's work still needs its recorded family (unknown fails closed).
        judged = writer if writer in MODEL_IDS or writer in EXACT_MODEL_PROVIDERS else owner
        if judged == owner and judged in ("pi", "antigravity"):
            family = model_family(judged, owner_model)
            if family == "unknown":
                raise ValueError("recorded main model family is unknown; independence is unverified")
            # Validated worker tiers contain native Codex/Claude models only.
            native_families = {"codex": "openai", "claude": "anthropic"}
            usable = sorted(usable, key=lambda c: native_families[c["provider"]] == family)
        else:
            usable = sorted(usable, key=lambda c: c["provider"] == judged)
    route = dict(usable[0], effort="low") if role == "researcher" else dict(usable[0])
    return dict(route, role=role, kind=KINDS[role], owner=owner, workload=tier, access=access, purpose=purpose)


def model_family(owner, model):
    """Use the registry classification for aggregators as well as native providers."""
    result = subprocess.run(["bash", "-c", '. "$1"; oms_provider_model_family "$2" "$3"',
                             "panel-model-family", str(ROOT / "scripts/lib/provider-registry.sh"),
                             owner, model or ""], capture_output=True, text=True, check=False,
                            timeout=10, stdin=subprocess.DEVNULL)
    return result.stdout.replace("\r", "").strip() if result.returncode == 0 else "unknown"


def alternate_route(route, installed=None, provider_status=None, repo=None):
    """Return the first eligible alternate provider from this exact tier, if any."""
    tier = route.get("workload")
    if tier not in TIERS:
        return None
    candidates = [candidate for candidate in policy()["tiers"][tier]
                  if candidate["provider"] != route.get("provider")
                  and (installed is None or candidate["provider"] in installed)
                  and (weekly_used(provider_status, candidate["provider"]) or 0) < 90
                  and not daily_capped(provider_status, candidate)]
    if route.get("role") in INDEPENDENT or KINDS.get(route.get("role")) == "advisor":
        candidates = [candidate for candidate in candidates if candidate["provider"] in MODEL_IDS]
    if route.get("role") == "researcher":
        candidates = [candidate for candidate in candidates
                      if route_family(candidate["provider"], candidate["model"])
                      and route_family(candidate["provider"], candidate["model"])[0] in RESEARCH_FAMILIES]
    candidates = public_candidates(candidates, repo)
    if not candidates:
        return None
    return dict(candidates[0], effort=route.get("effort"), role=route.get("role"),
                kind=route.get("kind"), owner=route.get("owner"), workload=tier,
                access=route.get("access"), purpose=route.get("purpose"))


def command(entry, repo, route, prompt=None, brief=None, verify=None, task_id=None, resume=None):
    family = route_family(route["provider"], route["model"])
    if route["role"] == "researcher" and (route["access"] != "read" or route["purpose"] != "research"
            or route["effort"] != "low" or not family or family[0] not in RESEARCH_FAMILIES
            or verify or resume):
        raise ValueError("researchers are pinned cheap read-only lookups, without gates or continuation")
    if bool(prompt) == bool(brief):
        raise ValueError("provide exactly one prompt or scoped brief file")
    if route["access"] == "write" and (not brief or not verify or not verify.strip()):
        raise ValueError("write roles need a scoped brief file and verifier")
    if resume and route["role"] not in WRITERS:
        raise ValueError("only write-capable roles continue a session; judges keep a context apart from the implementer")
    if route["role"] == "reviewer" and (not verify or not verify.strip()):
        raise ValueError("reviewers need a verifier")
    brief_text = None
    if brief:
        path = Path(brief).expanduser()
        brief = path if path.is_absolute() else Path(repo) / path
        if not brief.is_file() or brief.stat().st_size > 65536:
            raise ValueError("brief must be a regular file of at most 64 KiB")
        brief_text = brief.read_text(encoding="utf-8")
        if "\0" in brief_text:
            raise ValueError("brief contains a NUL character")
    prefix = ["bash", str(entry)]
    request = ["--prompt", prompt] if prompt else ["--prompt-file", str(brief)]
    effort = ["--reasoning-effort", route["effort"]] if route["effort"] and route["provider"] != "opencode" else []
    model = ([] if route["model"] == "provider-default" else ["--model", route["model"]]) + effort
    if route["role"] == "advisor":
        if verify:
            raise ValueError("advisors run no verifier; dispatch a reviewer or verifier to run checks")
        return prefix + ["advise", "--repo", str(repo), "--to", route["provider"],
                         "--strategy", "advisor", "--no-failures", "--no-task", "--no-memory"] + request + model
    if route["role"] == "reviewer":
        target = route["provider"] + (":model=" + route["model"] if route["model"] != "provider-default" else "")
        if brief:
            request = ["--prompt", brief_text]
        return prefix + ["peer-review", "--repo", str(repo), "--providers", target,
                         "--gate", "--verify", verify] + request + effort
    if verify and route["access"] == "read":
        raise ValueError("only reviewers run --verify among read roles; dispatch a reviewer to run checks")
    request = ["--brief-file", str(brief)] if brief else ["--prompt", prompt]
    profile = route["role"] if route["role"] != "worker" else (
        "implementation-worker" if route["purpose"] == "implement" else "analysis-worker")
    result = prefix + ["peer-delegate", "--repo", str(repo), "--to", route["provider"], "--role", profile,
                       "--no-memory", "--no-task", "--graph-context"] + request + model
    if route["purpose"] == "investigate" and route["access"] == "write":
        result += ["--allow-no-change"]
    if route["access"] == "read":
        result += ["--read-only"]
    if verify and verify.strip():
        result += ["--verify", verify]
    if task_id:
        result += ["--task-id", task_id]
    if resume:
        result += ["--resume-session", resume]
    return result
