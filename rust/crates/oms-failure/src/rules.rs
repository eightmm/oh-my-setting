use super::{Properties, Rule};

pub(super) const RULES: &[Rule] = &[
    Rule {
        code: "guard_parent_interference",
        pattern: r"worker-authority|worker guard stopped this run",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "inspect_guard_evidence_and_redispatch",
        },
    },
    Rule {
        code: "provider_capacity",
        pattern: r"capacity|overloaded|rate.?limit|temporarily unavailable",
        properties: Properties {
            retryable: true,
            fallback_allowed: true,
            context_escalation_allowed: false,
            recovery: "one_explicit_fallback",
        },
    },
    Rule {
        code: "provider_timeout",
        pattern: r"timeout|timed out|wall clock",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "resize_task_or_budget",
        },
    },
    Rule {
        code: "read_guard_changed",
        pattern: r"read guard rejected completed",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "rerun_review",
        },
    },
    Rule {
        code: "outbound_context_refused",
        pattern: r"outbound context refused|sensitive-looking content",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "remove_sensitive_context",
        },
    },
    Rule {
        code: "provider_no_answer",
        pattern: r"seat returned no answer",
        properties: Properties {
            retryable: true,
            fallback_allowed: true,
            context_escalation_allowed: false,
            recovery: "retry_or_drop_seat",
        },
    },
    Rule {
        code: "provider_policy_decline",
        pattern: r"policy decline|refus(?:e|al)|safety policy",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "terminal",
        },
    },
    Rule {
        code: "provider_auth",
        pattern: r"authentication|unauthorized|forbidden|credential.*missing|login required",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "repair_authentication",
        },
    },
    Rule {
        code: "context_too_large",
        pattern: r"context.*(?:large|limit|length)|too many tokens",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "reduce_context",
        },
    },
    Rule {
        code: "context_insufficient",
        pattern: r"missing context|insufficient context|unknown symbol|not enough information",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: true,
            recovery: "escalate_context",
        },
    },
    Rule {
        code: "contract_invalid",
        pattern: r"contract.*invalid|acceptance-vacuous|unadmittable|structurally impossible",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "repair_contract",
        },
    },
    Rule {
        code: "environment_unready",
        pattern: r"missing required|not installed|daemon.*unreachable|environment.*unready|no such file or directory",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "repair_environment",
        },
    },
    Rule {
        code: "verifier_mutated",
        pattern: r"verifier.*(?:mutat|chang)|verification surface|test reduction",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "reject_patch",
        },
    },
    Rule {
        code: "verifier_failed",
        pattern: r"verif(?:y|ication).*fail|test.*fail|acceptance.*fail|gate failed",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "analyze_failure",
        },
    },
    Rule {
        code: "scope_violation",
        pattern: r"scope.*violation|outside.*allowed|forbidden path",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "reject_patch",
        },
    },
    Rule {
        code: "authority_mismatch",
        pattern: r"authority.*mismatch|lease.*mismatch|approval.*mismatch|soul.*mismatch",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "verify_state",
        },
    },
    Rule {
        code: "state_conflict",
        pattern: r"state.*conflict|compare-and-set|cas|concurrent|rotated during",
        properties: Properties {
            retryable: true,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "refresh_and_reconcile",
        },
    },
    Rule {
        code: "remote_outcome_unknown",
        pattern: r"remote.*unknown|push.*ambiguous|outcome.*unknown",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "do_not_replay_side_effect",
        },
    },
    Rule {
        code: "budget_exhausted",
        pattern: r"budget.*exhaust|token budget|retry budget|cycle cap",
        properties: Properties {
            retryable: false,
            fallback_allowed: false,
            context_escalation_allowed: false,
            recovery: "preserve_partial_state",
        },
    },
];
