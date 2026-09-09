"""Reviewed task routes are complete overrides, never cross-provider defaults."""

import argparse
import json
import re
import sys
import unicodedata


FIELDS = {"provider", "model", "fallback_model", "reasoning_effort", "workload"}
DEFAULTS = {"model": "", "fallback_model": "", "reasoning_effort": "auto",
            "workload": "standard"}
EFFORTS = {"auto", "low", "medium", "high", "xhigh", "max", "ultra"}
# CLI aliases from provider-registry.sh are conveniences, not persisted IDs.
TRANSPORT_ALIASES = {"agy", "cursor-agent", "grok-build", "gemini-cli", "qwen-code",
                     "opencode2", "dsh", "deepseek-harness", "mistral-vibe",
                     "github-copilot", "factory-droid"}


def validate(value):
    if not isinstance(value, dict):
        raise ValueError("assignment must be an object")
    if not value:
        return {}
    if set(value) - FIELDS or "provider" not in value:
        raise ValueError("assignment requires provider and only model/fallback_model/reasoning_effort/workload")
    for key, item in value.items():
        if not isinstance(item, str) or any(
                unicodedata.category(ch) in {"Cc", "Cf", "Cs"} for ch in item):
            raise ValueError("assignment %s must be text without control characters" % key)
    provider = value["provider"]
    if (not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,63}", provider)
            or ".." in provider or provider in TRANSPORT_ALIASES):
        raise ValueError("assignment provider must be a canonical transport identifier")
    for key in ("model", "fallback_model"):
        model = value.get(key, "")
        if (len(model) > 160 or model.startswith("-") or model != model.strip()
                or model == "provider-default"):
            raise ValueError("invalid assignment %s" % key)
    if value.get("workload", "standard") not in {"standard", "routine"}:
        raise ValueError("assignment workload must be standard or routine")
    if value.get("reasoning_effort", "auto") not in EFFORTS:
        raise ValueError("invalid assignment reasoning_effort")
    return dict(value)


def resolve(task, defaults):
    assignment = validate(task.get("assignment", {}))
    if assignment:
        return dict(DEFAULTS, **assignment)
    return dict(DEFAULTS, **defaults)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--task-json", required=True)
    parser.add_argument("--provider", required=True)
    parser.add_argument("--model", default="")
    parser.add_argument("--fallback-model", default="")
    parser.add_argument("--reasoning-effort", default="auto")
    parser.add_argument("--workload", default="standard")
    args = vars(parser.parse_args())
    try:
        task = json.loads(args.pop("task_json"))
        if not isinstance(task, dict):
            raise ValueError("task must be an object")
        print(json.dumps(resolve(task, args), sort_keys=True))
    except (ValueError, TypeError) as exc:
        sys.stderr.write("error: %s\n" % exc)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
