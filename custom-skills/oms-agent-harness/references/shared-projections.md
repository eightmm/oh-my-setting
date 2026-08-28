# Shared Decisions, Separate Authority

Use this reference when several commands inspect the same fact. Call the
intent-specific public command, but trust the canonical projection it consumes;
do not reconstruct the decision with `grep`, timestamps, or path-prefix tests.

| Fact | Canonical decision | Consumers | Never substitute |
|---|---|---|---|
| Auto-update schedule | Parse one owned scheduler block as `absent`, `valid`, or `malformed` | install, uninstall, status, update | A begin-marker grep. Mutations must preserve malformed input and refuse. |
| Repository scope | Normalize once, then apply the same Bash-compatible literal/glob membership and forbidden precedence | change guard, proposal validation, admission, autonomy fences | Ad hoc string-prefix checks or assuming two different glob spellings are subset-related. Admission and landing still retain separate authority. |
| Provider identity and preference | Provider registry canonicalization, availability, then explicit target > configured preference > automatic peer | consult and advisor routing | Raw binary names or duplicated `agy` alias handling. Council and advisor semantics remain distinct. |
| `PROJECT.md` state | Parse to `missing`, `draft`, `confirmed`, `legacy-active`, or `invalid` | init, doctor, plan creation, autopilot | Treating every non-draft spelling as confirmed. Only intent adoption publishes confirmation. |
| Failure attention | Project unresolved rows to `actionable` or TTL-bound `retiring` | fail ledger, state, inbox, resume, runtime next | Counting all unresolved rows as blockers. Resolution remains an explicit append. |
| Approval availability | Preserve durable state and derive one read-time effective state | approval list/show, state, inbox, consumption checks | Reinterpreting durable `--state` as effective state, or presenting an expired approved grant as pending. Use the effective-state filter for the read-time view; only the lifecycle writer appends expiry. |
| Active-task verification | `agent-task status` freshness over task receipt, verify command, and repository state | state, runtime, MCP views | Parsing only Goal and Next Step from the task Markdown. |
| Plan actionability | One snapshot for task count, non-vacuity, dependency readiness, and claim expiry anchor | agent-plan, state, inbox, runtime, autonomy loops | Treating an empty plan as completed or using only `claimed_at` when the canonical anchor has a fallback. |

## Agent rules

1. Read the narrow public projection first (`inbox`, `state`, runtime, or the
   domain `status`). Drill into raw history only for audit.
2. An effective read state never grants mutation authority. Use the owning
   lifecycle verb for expire, reclaim, resolve, admit, land, or install.
3. Treat malformed authority input as a typed refusal, not as absence. Preserve
   its bytes until the owning recovery path repairs it.
4. Keep compatibility text/JSON stable at public wrappers. Add shared internals
   beneath them instead of teaching callers another interpretation.
5. When a new consumer needs one of these facts, extend the canonical decision
   and its cross-consumer regression rather than copying the predicate.
