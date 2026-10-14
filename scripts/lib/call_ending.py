"""Classify terminal call evidence shared by board and Results projections."""

LIVE_STATES = {"starting", "working", "running", "verifying", "live marker"}


def call_ending(call):
    """Classify recorded terminal evidence without changing the lifecycle state."""
    state, code = call.get("state"), call.get("exit")
    # A call in review awaits its owner's decision; its exit is evidence for that decision, not an ending.
    if state in LIVE_STATES or state in {"queued", "idle", "manual", "quiet", "blocked", "review", "waiting_input", "waiting_approval", "presence unknown"}:
        return None
    reason = call.get("reason_code")
    if call.get("guard") or reason == "worker_authority_violation":
        return "guard_stop"
    if state == "cancelled" or code in {129, 130, 143} or reason in {"cancelled_by_owner", "manual_clear"}:
        return "cancelled"
    if state == "timed_out" or code == 124:
        return "timed_out"
    verify = call.get("verify_exit")
    if reason == "verification_failed" or code == 0 and (
            type(verify) is int and verify != 0 or call.get("verify_status") == "failed"):
        return "verify_failed"
    if type(code) is int and code != 0:
        # Only a recorded provider error names one; a missing artifact alone does not.
        return "provider_error" if call.get("provider_error_exit") is not None else "failed"
    return "failed" if state == "failed" else None
