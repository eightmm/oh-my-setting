# oms-runtime

The native runtime shares one parser and router across CLI, panel and MCP
consumers. Host adapters supply canonical producer facts; missing adapters
refuse without running a legacy fallback.

`oms runtime resources host` reads the actual kernel boot generation SHA-256.
`resources status` observes the selected shared state repository's explicit
`repository-host-local-observed` policy and separate Team, invocation, open
process/session and write-WIP counts. `configured: false` has null policy and
occupancy; it does not activate a default. `active` is the recorded domain state,
and `host_matches_policy` separately identifies a stale host binding. Neither
observation supplies a launch permit.

`resources activate --policy PATH` explicitly activates a policy before Main
starts. Relative policy paths use the caller's directory, then must remain
inside the resolved physical execution repository. The native producer accepts
only an owned, single-link regular file without symlink traversal, at most
64 KiB, and requires its host generation to match `resources host`. The policy
must explicitly specify every finite limit and reviewer headroom; the CLI
supplies no capacity defaults. A dispatch snapshot CAS refuses changed state
or unreconciled legacy execution/usage history and never deletes it. The boot
proof and mutation writer share an absolute deadline; root resolution happens
before that producer deadline.

Policy JSON requires `schema: 1`, `kind: "oms-resource-policy"`, the scope above,
nonempty `domain` and `epoch`, and the observed `host_generation_sha256`.
`max_teams`, `max_invocations`, `max_open_processes` and `max_write_wip` must be
positive integers. `reviewer_headroom` is a nonnegative integer strictly below
`max_invocations`. Unknown fields, unsupported scopes and malformed JSON refuse.

`resources hold --expected-policy-sha256 SHA` blocks new admission for the exact
current policy while preserving independent custody, usage and settlement.
Activation and hold refuse harness children and delegated descendants. Host and
status remain read-only for children. The common `--repo` and `--pretty` options
are supported.

Retirement and epoch recovery require native canonical decision validation.
They are internal checked producer APIs, with no CLI that treats supplied JSON,
artifact paths, worker exit or done/cleanup flags as retirement authority.
