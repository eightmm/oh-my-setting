"""Role allocation for panel-owned tasks; authority is independent of workload."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODEL_IDS = {"codex": {"gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-6-astra"},
             "claude": {"claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1", "claude-haiku-5-5"}}
TIERS = ("light", "routine", "deep", "frontier")
# Role strategy -> the board kind recorded as panel_role; acceptance, scopes and continuation key on the kind.
KINDS = {"worker": "worker", "builder": "worker", "refactorer": "worker", "documenter": "worker",
         "integrator": "worker", "performance-engineer": "worker", "release-engineer": "worker",
         "researcher": "researcher", "advisor": "advisor", "architect": "advisor", "critic": "advisor",
         "reviewer": "reviewer", "verifier": "reviewer", "security-auditor": "reviewer"}
WRITERS = {"worker", "builder", "refactorer", "documenter", "integrator", "performance-engineer"}
# Judges of another context's work prefer the provider other than the main's when the tier offers it.
INDEPENDENT = {"reviewer", "verifier", "security-auditor", "critic"}
RESEARCH_FAMILIES = {"luna", "haiku"}
POOL_DEFAULTS = {"max_rework_rounds": 2}


def route_family(provider, model):
    pattern = {"codex": r"gpt-(\d+(?:\.\d+)*)-(sol|luna|astra)",
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


def valid_route(route):
    return (isinstance(route, dict) and set(route) == {"provider", "model", "effort"}
            and isinstance(route.get("provider"), str) and route["provider"] in MODEL_IDS
            and isinstance(route.get("model"), str) and route["model"] in MODEL_IDS[route["provider"]]
            and isinstance(route.get("effort"), str) and route["effort"] in {"low", "medium", "high"})


def policy():
    value = json.loads((ROOT / "config/panel-routing.json").read_text(encoding="utf-8"))
    if (not isinstance(value, dict) or not {"schema", "main", "tiers", "roles"} <= set(value)
            <= {"schema", "main", "tiers", "roles", "pool", "review_backlog_warn"} or value.get("schema") != 2):
        raise ValueError("unsupported panel routing policy")
    if "review_backlog_warn" in value and (type(value["review_backlog_warn"]) is not int
                                             or value["review_backlog_warn"] < 0):
        raise ValueError("review backlog warning threshold must be a non-negative integer")
    pool = value.get("pool", {})
    if (not isinstance(pool, dict) or not set(pool) <= set(POOL_DEFAULTS)
            or any(type(number) is not int or number <= 0 for number in pool.values())):
        raise ValueError("panel pool limits must be positive integers")
    value["pool"] = dict(POOL_DEFAULTS, **pool)
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
        raise ValueError("panel roles must map every role to a tier and optional provider preference")
    for role, route in roles.items():
        if (not isinstance(route, dict) or not {"tier"} <= set(route) <= {"tier", "prefer"}
                or not isinstance(route.get("tier"), str) or route["tier"] not in TIERS
                or "prefer" in route and (not isinstance(route["prefer"], str) or route["prefer"] not in MODEL_IDS)):
            raise ValueError("invalid panel role: %s" % role)
    return value


def allocate(owner, role="worker", tier=None, access="read", purpose=None, installed=None, provider_status=None):
    """Availability, quota left, role preference, then independent judgment; explicit targets narrow installed."""
    if not owner or role not in KINDS:
        raise ValueError("choose a supported main and role")
    routes = policy()
    preferred = routes["roles"][role].get("prefer")
    tier = tier or routes["roles"][role]["tier"]
    if tier not in TIERS or access not in ("read", "write"):
        raise ValueError("invalid workload or access")
    if access == "write" and role not in WRITERS:
        raise ValueError("%s is read-only" % role)
    if role == "worker":
        purpose = purpose or ("implement" if access == "write" else "explain")
        if purpose not in ("explain", "investigate", "implement") or (purpose == "implement") != (access == "write"):
            raise ValueError("worker purpose must match access: explain or investigate reads, implement writes")
    elif purpose:
        raise ValueError("--purpose applies to plain workers; the role names the task")
    else:
        purpose = {"advisor": "advise", "reviewer": "review", "researcher": "research"}.get(role, role)
    candidates = routes["tiers"][tier]
    if role == "researcher":
        if tier != "light":
            raise ValueError("researchers use the light tier")
        candidates = [c for c in candidates if route_family(c["provider"], c["model"])[0] in RESEARCH_FAMILIES]
    usable = [c for c in candidates if installed is None or c["provider"] in installed]
    if not usable:
        raise ValueError("no installed provider serves the %s tier (%s); install one or use --to" % (
            tier, ", ".join(sorted({c["provider"] for c in candidates})) or "none configured"))
    if len(usable) > 1:
        # Use the board's rounded reading; stale, malformed and missing readings remain available.
        from panel_metrics import percent
        readings = provider_status if isinstance(provider_status, dict) else {}
        available, used_by = [], {}
        for candidate in usable:
            row = readings.get(candidate["provider"])
            used = percent(row.get("weekly_used")) if isinstance(row, dict) else None
            if used is None or used < 90:
                available.append(candidate)
                used_by[candidate["provider"]] = used
        if not available:
            raise ValueError("all installed providers in the %s tier have <=10%% weekly quota left" % tier)
        # With a reading for every remaining provider, the one with more weekly quota left goes first;
        # otherwise the configured tier order stands (sorted() keeps ties in that order).
        if None not in used_by.values():
            available = sorted(available, key=lambda c: used_by[c["provider"]])
        usable = available
    if preferred:
        usable = sorted(usable, key=lambda c: c["provider"] != preferred)
    if role in INDEPENDENT:
        usable = sorted(usable, key=lambda c: c["provider"] == owner)
    route = dict(usable[0], effort="low") if role == "researcher" else dict(usable[0])
    return dict(route, role=role, kind=KINDS[role], owner=owner, workload=tier, access=access, purpose=purpose)


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
    effort = ["--reasoning-effort", route["effort"]] if route["effort"] else []
    model = ([] if route["model"] == "provider-default" else ["--model", route["model"]]) + effort
    if route["role"] == "advisor":
        return prefix + ["advise", "--repo", str(repo), "--to", route["provider"],
                         "--no-failures", "--no-task", "--no-memory"] + request + model
    if route["role"] == "reviewer":
        target = route["provider"] + (":model=" + route["model"] if route["model"] != "provider-default" else "")
        if brief:
            request = ["--prompt", brief_text]
        return prefix + ["peer-review", "--repo", str(repo), "--providers", target,
                         "--gate", "--verify", verify] + request + effort
    request = ["--brief-file", str(brief)] if brief else ["--prompt", prompt]
    profile = route["role"] if route["role"] != "worker" else (
        "implementation-worker" if route["access"] == "write" else "analysis-worker")
    result = prefix + ["peer-delegate", "--repo", str(repo), "--to", route["provider"], "--role", profile,
                       "--no-memory", "--no-task"] + request + model
    if route["access"] == "read":
        result += ["--read-only"]
    else:
        result += ["--verify", verify]
    if task_id:
        result += ["--task-id", task_id]
    if resume:
        result += ["--resume-session", resume]
    return result
