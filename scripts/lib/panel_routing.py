"""Role allocation for panel-owned tasks; authority is independent of workload."""

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODEL_IDS = {"codex": {"gpt-6-sol", "gpt-6-luna", "gpt-6-astra"},
             "claude": {"claude-opus-5-5", "claude-sonnet-5-5", "claude-fable-5-1"}}


def policy():
    value = json.loads((ROOT / "config/panel-routing.json").read_text(encoding="utf-8"))
    if not isinstance(value, dict) or value.get("schema") != 1:
        raise ValueError("unsupported panel routing policy")
    for group, keys in (("main", ("codex", "claude")), ("worker", ("light", "routine")),
                        ("advisor", ("astra", "fable"))):
        for key in keys:
            route = value.get(group, {}).get(key, {}) if isinstance(value.get(group), dict) else {}
            if (not isinstance(route, dict) or route.get("provider") not in MODEL_IDS
                    or route.get("model") not in MODEL_IDS[route["provider"]]
                    or route.get("effort") not in {"low", "medium", "high"}):
                raise ValueError("invalid panel route: %s/%s" % (group, key))
    return value


def allocate(owner, role="worker", workload="routine", seat="auto", access="read", purpose=None):
    if not owner or role not in ("worker", "advisor", "reviewer"):
        raise ValueError("choose a supported main and role")
    if workload not in ("light", "routine", "main") or access not in ("read", "write"):
        raise ValueError("invalid workload or access")
    purpose = purpose or {"worker": "explain", "advisor": "advise", "reviewer": "review"}[role]
    if purpose not in ("explain", "investigate", "implement", "review", "advise"):
        raise ValueError("invalid task purpose")
    if seat not in ("auto", "astra", "fable"):
        raise ValueError("invalid advisor seat")
    if role != "worker" and access != "read":
        raise ValueError("advisors and reviewers are read-only")
    if access == "write" and purpose != "implement":
        raise ValueError("write workers require implement purpose")
    if ((role == "advisor" and purpose != "advise") or (role == "reviewer" and purpose != "review")
            or (role == "worker" and access == "read" and purpose not in ("explain", "investigate"))):
        raise ValueError("task purpose must match the role and access")
    if role == "worker" and seat != "auto":
        raise ValueError("advisor seats do not select worker models")
    routes = policy()
    if role in ("advisor", "reviewer"):
        chosen = seat if seat != "auto" else "astra" if owner == "claude" else "fable"
        route = routes["advisor"][chosen]
    else:
        if workload == "main" and owner not in routes["main"]:
            raise ValueError("additional CLI mains need an explicit --to route for main-level work")
        route = routes["main"][owner] if workload == "main" else routes["worker"][workload]
    return dict(route, role=role, owner=owner, workload=workload, access=access, purpose=purpose)


def command(entry, repo, route, prompt=None, brief=None, verify=None, task_id=None):
    if bool(prompt) == bool(brief):
        raise ValueError("provide exactly one prompt or scoped brief file")
    if route["access"] == "write" and (not brief or not verify or not verify.strip()):
        raise ValueError("write workers need a scoped brief file and verifier")
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
                       "--role", "implementation-worker" if route["access"] == "write" else "analysis-worker",
                       "--no-memory", "--no-task"] + request + model
    if route["access"] == "read":
        result += ["--read-only"]
    else:
        result += ["--verify", verify]
    if task_id:
        result += ["--task-id", task_id]
    return result
