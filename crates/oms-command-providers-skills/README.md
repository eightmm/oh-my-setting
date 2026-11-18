# Native provider and skill command crate

Standalone Rust 2024 crate with Rust 1.99.1 as its minimum toolchain. The
installation dispatcher supplies a separately verified source root to
`run_with_source`; `owns` only classifies command names and performs no I/O.
The workspace dispatcher, migration receipts and root lockfile are unchanged.

This is an implementation draft, **not a parity-complete migration**. Compilation
covers every route below, but does not establish source-contract equivalence.
Do not treat the compilation gate as authority to install this draft.

| Routes | Modules |
| --- | --- |
| `provider-permissions` | `permissions`, `common` |
| `connect-services` | `connect` |
| `models`, `model-doctor` | `models` |
| `harness` | `harness`, `broker`, `gateway`, `messages`, `https` |
| `skill-doctor`, `skill-forge` | `skills`, `lifecycle` |
| `apply-project-template`, `remove-project-template` | `project` |
| `project-doctor`, `project-private`, `detect-project-style`, `init` | `project`, `reports` |
| `generate-slurm-reference`, `agent-ml-context`, `import-agent-result` | `utilities` |

## Transport boundary

`HttpsTransport` permits an in-memory mock. `CurlTransport` starts only
`/usr/bin/curl`, disables its default configuration and inherited environment,
requires HTTPS and TLS 1.2, applies connection and request deadlines, and sends
to the fixed Codex subscription origin. Request headers are supplied through an
exclusive 0600 config inside a random 0700 directory; descriptor and pathname
identity, ownership, file type and link count are checked. Written buffers are
cleared and the config is removed on ordinary drop. A hard process kill can
leave a private request directory behind; it never makes cleanup run.

The loopback listener binds an ephemeral IPv4 loopback port. It checks Host,
Origin, duplicate headers, capability, endpoint and bounded Content-Length
before calling its backend. A Mutex/Condvar slot counter admits at most eight
connections. The capability comes from `/dev/urandom`; comparison processes the
whole compared input. SSE writes flush, and terminal events stop the upstream
reader before any socket trailer. Authentication-broker failures return fixed
messages rather than native diagnostic content. No bundle file runs during
skill preview, import, update, rollback or risk inspection. Evaluation commands
require `--allow-host-commands`.

## Persistent effects and interruption

| Written files or records | Concurrent call and interruption behavior |
| --- | --- |
| Antigravity settings, `.bak`, permission sidecar | One canonical settings lock serializes OMS writers. Single-file replacement is atomic. Settings publish before added ownership, so a kill can leave an unowned grant. Removal journals bind before/after digests; mismatching recovery preserves settings. Equal user-added duplicates survive single-occurrence revocation. |
| Managed loader files, `PROJECT.md`, ML scaffolds, optional CI, ML `.gitignore` | A template transaction lock serializes template calls; repository-root descriptors anchor byte replacement. Existing scaffold files are preserved. The file set is not atomic: a kill can leave partial provider parity; rerun or project-doctor exposes it. |
| Common Git `info/exclude`, optional Git index removal | Exclude writers share the common-directory lock, including linked worktrees. User text outside the managed block survives. `--untrack` stages index removal before exclude publication; interruption can leave that staged removal with the disk file intact. This command does not edit project `.gitignore`. |
| Local `SKILL.md`, admissions JSONL, provider discovery links | The skill-store authority lock serializes mutation. Add refuses a preexisting name and requires `oms-`; read paths accept legacy names. A kill can leave a stored skill without admission or with only some links; explicit admit/link repairs that state. Foreign discovery entries are preserved. |
| Imported immutable revisions and `lock.json` | Revision contents are copied and digest-checked before publication. The authority lock serializes pointer changes. A kill can leave quarantine directories or unreferenced revisions; the previous pointer remains until replacement. Link changes across provider roots are separate from pointer publication. |
| Provider account registry and ownership receipts (`codex`, `claude`, `pi`) | The registry lock serializes calls. Directory identity binds link changes. Codex/Pi record intent in `codex-account-links.json` / `pi-account-links.json` before creating each link; Claude records `provider-account-effects.json` (home, parent and link identities) after creating each link, then `provider-accounts.claude.json`, then `provider-accounts.json`. Removal retires receipt entries after unlink and the registry before the receipt. A kill can leave pending ownership or links before registry publication (an unrecorded Claude link is never removed by uninstall). Homes, login stores and sessions are preserved. |
| Capability `.env`, `.models`, `.efforts` files | Per-provider snapshot locks serialize writers. Each file replaces atomically; readers can see different generations across the three files after interruption. No cross-file atomic snapshot is claimed. |
| Slurm reference and optional raw snapshots | One reference lock covers publication and raw capture. Each file is atomic. A kill between raw writes can leave mixed capture generations. No submission command runs. |
| Imported answer artifact and artifact-index row | The index lock covers artifact publication before row publication. Random artifact names keep concurrent imports distinct. A kill can leave an unindexed artifact; a published row follows its completed artifact write. External artifacts are marked unowned. |
| Memory initialized by `init` | Delegated to the existing native memory command, including its own storage and index contracts. |

Independent non-OMS editors are not serialized by these locks. Byte mutation
uses observed-change checks; no filesystem snapshot guarantee is claimed.
Subprocess output and elapsed time are bounded, with process-group cleanup.

## Remaining implementation gaps

- Account isolation is proved only for env-isolated `codex` (`CODEX_HOME`),
  `claude` (`CLAUDE_CONFIG_DIR`) and `pi` (`PI_CODING_AGENT_DIR`).
  `connect-services --account PROVIDER:ID` registers, `--repair`s, `--remove`s
  and `--verify`s them; any other provider, including `antigravity`, is refused
  before mutation. A canonical home is unique across providers; the same id may
  exist under different providers. Claude hooks inside an account home are
  installed separately by `install-claude-hooks --ownership-receipt`, and
  removal of an account with installed hooks is refused until they are removed.
  `oms doctor` reports account home safety and receipt health read-only.
- Project diagnostics are bounded native implementations, but full source
  parity remains unfinished: team digest comparison, complete contract
  evidence, prompt sentence duplication, exact budget normalization and all
  ML structure/notebook diagnostics need completion.
- Skill diagnostics still need the complete legacy-name, plugin-overlap,
  near-duplicate description and live Antigravity loaded-skill checks. Imported
  status/template-drift and outdated-report details also need parity review.
- Model inventory does not dispatch the legacy detached daily stale-catalog
  refresh. Native Pi launch does not register the panel child identity. `init`
  does not register the optional tick service or reproduce every checklist line.
- The gateway bounds upstream waiting through curl's request deadline; active
  gateway shutdown does not yet cancel a blocked upstream read immediately.
  Structured quota reset propagation and the complete upstream error taxonomy
  need comparison with the source implementation.
- New project-template target directories currently must already exist. The
  source scaffold also supports creating a missing target directory.
- Full output text, schema, permission/ownership race and crash parity remain
  unverified. No behavior test or live provider/network request was executed.

## Verification performed

The offline all-targets Cargo check and rustfmt check ran successfully on
Rust 1.99.1. Tests were deliberately skipped under the implementation-first
brief. No test was added or modified, so reverted-code test-failure proof was
not applicable. Existing consumer references were searched; their tests remain
for the subsequent behavior-verification phase.
