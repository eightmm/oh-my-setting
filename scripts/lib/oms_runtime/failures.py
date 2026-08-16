"""Canonical failure taxonomy and deterministic recovery metadata."""

from __future__ import annotations

import re
from typing import Any, Dict, List, Mapping, Optional, Tuple

from .common import bounded_line

FAILURE_RULES: List[Tuple[str, re.Pattern[str], Dict[str, Any]]] = [
    ("provider_capacity", re.compile(r"capacity|overloaded|rate.?limit|temporarily unavailable", re.I), {"retryable": True, "fallback_allowed": True, "context_escalation_allowed": False, "recovery": "one_explicit_fallback"}),
    ("provider_timeout", re.compile(r"timeout|timed out|wall clock", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "resize_task_or_budget"}),
    ("provider_policy_decline", re.compile(r"policy decline|refus(?:e|al)|safety policy", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "terminal"}),
    ("provider_auth", re.compile(r"authentication|unauthorized|forbidden|credential.*missing|login required", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "repair_authentication"}),
    ("context_too_large", re.compile(r"context.*(?:large|limit|length)|too many tokens", re.I), {"retryable": True, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "reduce_context"}),
    ("context_insufficient", re.compile(r"missing context|insufficient context|unknown symbol|not enough information", re.I), {"retryable": True, "fallback_allowed": False, "context_escalation_allowed": True, "recovery": "escalate_context"}),
    ("contract_invalid", re.compile(r"contract.*invalid|acceptance-vacuous|unadmittable|structurally impossible", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "repair_contract"}),
    ("environment_unready", re.compile(r"missing required|not installed|daemon.*unreachable|environment.*unready|no such file or directory", re.I), {"retryable": True, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "repair_environment"}),
    ("verifier_mutated", re.compile(r"verifier.*(?:mutat|chang)|verification surface|test reduction", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "reject_patch"}),
    ("verifier_failed", re.compile(r"verif(?:y|ication).*fail|test.*fail|acceptance.*fail", re.I), {"retryable": True, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "analyze_failure"}),
    ("scope_violation", re.compile(r"scope.*violation|outside.*allowed|forbidden path", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "reject_patch"}),
    ("authority_mismatch", re.compile(r"authority.*mismatch|lease.*mismatch|approval.*mismatch|soul.*mismatch", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "verify_state"}),
    ("state_conflict", re.compile(r"state.*conflict|compare-and-set|\bCAS\b|concurrent|rotated during", re.I), {"retryable": True, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "refresh_and_reconcile"}),
    ("remote_outcome_unknown", re.compile(r"remote.*unknown|push.*ambiguous|outcome.*unknown", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "do_not_replay_side_effect"}),
    ("budget_exhausted", re.compile(r"budget.*exhaust|token budget|retry budget|cycle cap", re.I), {"retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "preserve_partial_state"}),
]


def codes() -> List[str]:
    return [code for code, _pattern, _properties in FAILURE_RULES] + ["unknown_failure"]


def classify(text: str, exit_code: Optional[int] = None, explicit: str = "") -> Dict[str, Any]:
    if explicit:
        for code, _pattern, properties in FAILURE_RULES:
            if explicit == code:
                return dict(properties, code=code, matched="explicit", exit=exit_code)
    if exit_code == 124:
        return {"code": "provider_timeout", "retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "resize_task_or_budget", "matched": "exit-124", "exit": exit_code}
    for code, pattern, properties in FAILURE_RULES:
        match = pattern.search(text or "")
        if match:
            return dict(properties, code=code, matched=bounded_line(match.group(0), 120), exit=exit_code)
    return {"code": "unknown_failure", "retryable": False, "fallback_allowed": False, "context_escalation_allowed": False, "recovery": "preserve_and_escalate", "matched": "", "exit": exit_code}


def catalog() -> Dict[str, Mapping[str, Any]]:
    return {code: dict(properties) for code, _pattern, properties in FAILURE_RULES}
