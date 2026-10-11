"""Role allocation for panel-owned tasks; authority is independent of workload."""

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODEL_IDS = {"codex": {"gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-6-astra"},
             "claude": {"claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"}}


def research_version(provider, model):
    pattern = {"codex": r"gpt-(\d+(?:\.\d+)*)-luna", "claude": r"claude-haiku-(\d+(?:-\d+)*)"}.get(provider)
    match = re.fullmatch(pattern, model) if pattern and isinstance(model, str) else None
    if not match:
        return None
    version = tuple(int(part) for part in re.split(r"[.-]", match[1]))
    while len(version) > 1 and version[-1] == 0:
        version = version[:-1]
    return version


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


def newest_research_model(provider, configured, catalog):
    candidates = {configured, *(item for item in catalog if research_version(provider, item))}
    if not research_version(provider, configured):
        raise ValueError("researchers require a versioned Luna or Haiku model")
    return max(candidates, key=lambda model: (research_version(provider, model), model == configured, model))


def policy():
    value = json.loads((ROOT / "config/panel-routing.json").read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise ValueError("unsupported panel routing policy")
    for group, keys in (("main", ("codex", "claude")), ("worker", ("light", "routine")),
                        ("advisor", ("astra", "fable")), ("researcher", ("codex", "claude"))):
        for key in keys:
            route = value.get(group, {}).get(key, {}) if isinstance(value.get(group), dict) else {}
            if (not isinstance(route, dict) or route.get("provider") not in MODEL_IDS
                    or (not research_version(route["provider"], route.get("model")) if group == "researcher"
                        else route.get("model") not in MODEL_IDS[route["provider"]])
                    or route.get("effort") not in {"low", "medium", "high"}):
                raise ValueError("invalid panel route: %s/%s" % (group, key))
            if group == "researcher" and (route["provider"] != key or route["effort"] != "low"):
                raise ValueError("researchers retain their provider family and low effort")
    return value


def allocate(owner, role="worker", workload="routine", seat="auto", access="read", purpose=None):
    if not owner or role not in ("worker", "advisor", "reviewer", "researcher"):
        raise ValueError("choose a supported main and role")
    if workload not in ("light", "routine", "main") or access not in ("read", "write"):
        raise ValueError("invalid workload or access")
    purpose = purpose or {"worker": "explain", "advisor": "advise", "reviewer": "review", "researcher": "research"}[role]
    if purpose not in ("explain", "investigate", "implement", "review", "advise", "research"):
        raise ValueError("invalid task purpose")
    if seat not in ("auto", "astra", "fable"):
        raise ValueError("invalid advisor seat")
    if role != "worker" and access != "read":
        raise ValueError("advisors and reviewers are read-only")
    if role == "researcher" and (purpose != "research" or seat != "auto" or workload == "main"):
        raise ValueError("researchers require research purpose and a cheap read-only route")
    if role != "researcher" and purpose == "research":
        raise ValueError("research purpose requires researcher role")
    if access == "write" and purpose != "implement":
        raise ValueError("write workers require implement purpose")
    if ((role == "advisor" and purpose != "advise") or (role == "reviewer" and purpose != "review")
            or (role == "worker" and access == "read" and purpose not in ("explain", "investigate"))):
        raise ValueError("task purpose must match the role and access")
    if role == "worker" and seat != "auto":
        raise ValueError("advisor seats do not select worker models")
    routes = policy()
    if role == "researcher":
        route = routes["researcher"][owner if owner in routes["researcher"] else "codex"]
        workload = "light"
    elif role in ("advisor", "reviewer"):
        chosen = seat if seat != "auto" else "astra" if owner == "claude" else "fable"
        route = routes["advisor"][chosen]
    else:
        if workload == "main" and owner not in routes["main"]:
            raise ValueError("additional CLI mains need an explicit --to route for main-level work")
        route = routes["main"][owner] if workload == "main" else routes["worker"][workload]
    return dict(route, role=role, owner=owner, workload=workload, access=access, purpose=purpose)


def command(entry, repo, route, prompt=None, brief=None, verify=None, task_id=None, resume=None):
    if route["role"] == "researcher" and (route["access"] != "read" or route["purpose"] != "research"
            or route["effort"] != "low" or not research_version(route["provider"], route["model"])
            or verify or resume):
        raise ValueError("researchers are pinned cheap read-only lookups, without gates or continuation")
    if bool(prompt) == bool(brief):
        raise ValueError("provide exactly one prompt or scoped brief file")
    if route["access"] == "write" and (not brief or not verify or not verify.strip()):
        raise ValueError("write workers need a scoped brief file and verifier")
    if resume and route["role"] != "worker":
        raise ValueError("only workers continue a session")
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
    result = prefix + ["peer-delegate", "--repo", str(repo), "--to", route["provider"],
                       "--role", "researcher" if route["role"] == "researcher" else
                       "implementation-worker" if route["access"] == "write" else "analysis-worker",
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
