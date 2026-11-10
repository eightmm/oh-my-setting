# Pinned source intake

`oms intent draft` can use one explicitly selected repo-local text file or
GitHub.com issue as input to its existing reviewed-contract workflow.
This implements F8 / SPEC-G10 / REV-G16 without another task store or planner.

```bash
oms intent draft --from-file requirements.md --dry-run
oms intent draft --from-file requirements.md --source-sha256 REVIEWED_SHA --goal 'Bounded operator goal'
oms intent draft --from-issue https://github.com/example/project/issues/12 --dry-run
oms intent draft --from-issue https://github.com/example/project/issues/12 --source-sha256 REVIEWED_SHA
```

The operating agent reads the original source and its preview, then supplies
the exact reviewed SHA. The person does not need to copy commands or inspect
`.oms`. A preview is not approval of the source's scope, commands, links or
checkboxes. The later candidate still requires the ordinary review and
`intent adopt` gates; no source is adopted, claimed or dispatched automatically.

## Flow and provenance

`--dry-run` returns a schema-1 `oms-intent-source` JSON object: kind, reference,
revision, SHA-256 and byte count, plus `trusted: false`. It omits source text,
creates no OMS candidate, and makes no model call. With an issue it does make
one explicit read through the user's existing `gh` authentication.

A non-preview source draft requires `--source-sha256`. The source is read and
checked before a provider call. Its frozen text is enclosed as JSON data in an
untrusted-input section; operator instructions and the repository's current
contract take precedence. Conflicts and absent scope/checks remain open
questions, not new authority. The existing `agent-call` outbound scanner,
provider routing, answer extraction and candidate validation are unchanged.

After the answer passes validation, the source is read again with the same
pin before candidate publication. Drift or unavailability refuses publication
and retains the provider artifact for diagnosis. A successful candidate records
source metadata in its Provenance section, not a second copy of the source body.
A source that changes and changes back between checks is not detected. This is
a cooperative revision fence, not an atomic transaction with an external service
or a hostile same-UID filesystem. Prompt labels are not a sandbox or a guarantee
that a model will ignore prompt injection.

`--goal` remains an optional operator-supplied constraint for source drafts.
Without it the goal is to prepare a reviewable contract from the pinned source
and leave unresolved choices open. Goal-only draft/show/adopt retain their
existing behavior. Source options on show/adopt or both source selectors are
rejected. A marked harness child may preview, but cannot publish a new
source-backed candidate.

## Input and resource limits

Files must be regular, single-link, own-UID files within `--repo`. Every path
component is opened without following symlinks. Parent traversal, special
files, detected in-place writes or replacement during read are refused.
The source must be nonempty UTF-8, at most **32 KiB**; unsupported control and
format characters are rejected rather than silently changed. Paths have at
most 64 components. The source cap also bounds JSON expansion passed to the
existing prompt argument; it is not a token or billing limit.

GitHub input is an exact HTTPS `github.com/OWNER/REPO/issues/NUMBER` URL. Query
strings, fragments, PR URLs, other hosts and redirect-like identities are
rejected. The response must match the requested issue's ID/number/URL, not a
pull request. Its title and body share the source byte limit. The hash covers
issue ID, number, canonical URL, updated time, title and body; updated_at alone
is not trusted to identify unchanged content. Comments, attachments and linked
pages are not fetched.

Each check runs one argv-based `gh api --hostname github.com --method GET`
request with no stdin or shell, at most 512 KiB of response and 20 seconds.
It does not prompt, retry, fall back to another host, modify remote issues or
install/login `gh`. Errors, stderr and credentials are not echoed. Process
cleanup is limited to the helper's own spawned process group. Preview uses
one GET, a successful draft normally two; provider retries reuse the frozen
source. The installed `gh` program owns its own authentication behavior.

Private references and source text remain private metadata/input. The preview
is not a secret-redaction service. Outbound drafting uses the normal scanner;
it does not bypass that scanner to accept source content. No real provider or
GitHub account is required by the synthetic tests.

## Exit behavior and verification

Bad source, stale pin, invalid option combinations and source drift before
publication return exit 2. Existing draft transport/validation failures retain
their exit behavior. A successful metadata query does not mean that a contract
was created or that a requirement was verified.

`tests/test_intent_source.py` covers the real reader, hash/identity checks,
resource bounds and a private fake-gh process. Its front-door cases run the
actual shell entrypoint/helper with unrelated library helpers and provider
transport stubbed, checking preview, draft, drift, child refusal and unchanged
goal-only routing. These are not full installed-provider tests.

The existing `tests/context-core-smoke.sh` registers the tests with `--cli`.
That adds a preview through the real repository shell libraries and a private
HOME, asserting no repository/HOME content changes. No new CI lane is added.
Full draft/adopt regression coverage remains in the existing intent fixtures.

Rollback reverts this scoped change. Existing candidates and PROJECT.md need
no migration; existing readers ignore the additional provenance bullet.
