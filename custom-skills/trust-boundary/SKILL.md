---
name: trust-boundary
description: >
  Threat-model a change by tracing assets, actors, trust boundaries, abuse
  paths, controls, and verification evidence. Use only when the user explicitly
  requests a threat model, abuse-path analysis, trust-boundary review, or
  security release gate. Do not invoke for routine authentication, input, API,
  secret, or internal implementation work.
---

# Trust Boundary Review

Review the concrete data and privilege flow, not a generic checklist. Keep a
review request read-only; implement fixes only when the user asked for changes.

## Map the boundary

1. Identify the protected asset, actors, entry points, privileged effects, and
   every trust boundary crossed. State consequential assumptions.
2. Trace attacker-controlled data to its sinks. Check validation at the
   boundary, context-safe query/command/path/rendering APIs, size and resource
   limits, and canonicalization where alternate spellings matter.
3. Separate authentication from authorization. Enforce authorization on the
   server for the specific action, object, owner or tenant; do not infer it
   from UI state or possession of an identifier.
4. Follow secrets and sensitive data through config, storage, transport, logs,
   errors, caches, backups, and third parties. Prefer least privilege, bounded
   lifetime, redaction, and fail-closed behavior.
5. Consider replay, duplicate delivery, CSRF, races, confused-deputy paths,
   enumeration, and resource exhaustion only where the architecture makes
   them plausible. Do not prescribe controls for threats that cannot reach the
   boundary.

Record the result compactly as `asset -> boundary -> abuse path -> control ->
evidence`. Rank findings by realistic exploitability and impact. Keep observed
code and configuration separate from inference or missing deployment facts.

## Verify the control

Use the smallest authorized local or test-environment probes. Include relevant
negative-path cases: unauthenticated access, the wrong user or tenant,
malformed or oversized input, expired or missing credentials, replay or
duplicate requests, and concurrent state changes. Check that denial is
fail-closed and that logs and responses do not expose secrets or internals.

Never probe a live third-party or production system without explicit authority.
For review output, report actionable findings first with file/line evidence,
then residual risk, assumptions, and skipped or impossible checks. If no
finding survives verification, say so plainly.
