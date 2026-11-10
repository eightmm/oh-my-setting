# Recorded run input identity

A run capsule and its companion ledger row now share the inputs and metric
contract captured **before** the command. This joins the capsule data/config
metadata with the ledger's metric-contract and seed grouping. It does not run
another experiment or replace ExperimentContract.

## Captured, not inferred

The capsule adds `input_identity` and `metric_contract` under schema 1. Its
companion ledger row adds the same `input_identity`, `capsule_id`, and
`seed_status`. The companion uses the captured commit/dirty fingerprint and
metric records rather than relabelling a completed run with files changed by
the command. A missing or invalid metric contract at capture stays unrecorded;
a contract created later does not retroactively label that run.

The identity hashes a bounded projection of recorded code, declared config
fingerprints and declared data-manifest fingerprints. It stores hashes and
coverage counts, not config values, source paths or raw data, in the tracked
ledger. Config identity is `incomplete` when a declared config was not captured.
No configs or datasets declared means `unrecorded`, not proof of no such input.

The existing capsule fingerprints are reused: config hashes may be 32-hex
SHA-256 prefixes and dirty-tree hashes 16-hex prefixes. Hashing those records
again does not restore missing precision. A manifest digest describes recorded
metadata, not a fresh check of the underlying split files or leakage policy.
Sources can change during execution; this feature does not create an atomic
snapshot or prove which inputs a program actually used. Same-UID code can forge
local records. Identity is consistency metadata, never an approval or security
boundary.

## Ranking and replication

`run-ledger top` retains its metric-contract filtering and additionally refuses
to rank distinct recorded data identities, or known versus unrecorded data,
unless `--mixed` is explicit. The refusal is exit 4 (`not comparable`). Invalid
identity rows are counted and excluded when not mixed. Old ledgers with no
identities preserve their existing behavior; absence is not completeness.

Different configurations may be compared as treatments on the same recorded
data, but cannot be pooled as seed replicates. `--by-group` keys replicas by
the group label, the entire recorded input identity and the metric contract.
Even `--mixed` cannot pool different input/metric scopes. A reused group label
is displayed with a scope suffix when necessary. Missing declared configs,
invalid identities and unavailable code identity do not form replica groups.

A rerun replaces the previous value of the same seed **in ledger append
order**, before score sorting. File order is the defined order, not a claim
that remote timestamps establish chronology. Filtering still selects usable
metric rows according to the existing exit policy; this is not an automatic
quality or significance verdict. Bootstrap calculations are unchanged.

A single numeric capsule seed is carried to the companion. Multiple distinct
seeds, unrecognized declarations, or a declared seed conflicting with the
command's recorded seed are marked and not converted into a scalar replicate.
A seed stored inside a config is not heuristically removed from that config's
hash: differently hashed config files remain separate scopes.

## Compatibility and limits

Existing direct ledger runs, goal/plan/evidence authority and public flags are
unchanged. The optional internal `OMS_RUN_LEDGER_CAPTURED` handoff is accepted
only with the existing completed-command override. It is schema checked,
limited to 64 KiB, and cannot introduce arbitrary fields. It is not a public
permission mechanism. The capsule passes pre-command data without re-running
the command. If companion recording fails, the capsule remains available.

Capture supports at most 128 config/data identities and seed declarations.
Inconsistent or oversized captured metadata refuses before command execution.
The capsule may already have allocated its local bundle at that point. Existing
capsule privacy checks remain; this feature is not a secret-redaction service.

Rollback reverts this change. Old readers ignore the additional fields. An old
reader will not enforce the new data-comparability and replica-scope checks.
No automatic migration, scheduling, training, retry, merge or installation is
added. Full ExperimentContract/holdout/claims integration is separate work.

## Tests

`tests/test_oms_runtime_run_identity.py` is discovered by the existing
`runtime-core-smoke.sh`. Pure tests cover stable fingerprints, incomplete and
malformed records, data/config separation, prefix collisions, captured contract,
ambiguous seed, latest rerun and scope-aware grouping. Shell tests use the real
capsule/ledger scripts and helper, with isolated stubs only for environment
probes, lock/privacy/journal adapters and unchanged bootstrap calculation.
They verify pre-command capture despite source changes, single execution,
invalid handoff refusal, legacy behavior and both ranking directions.

These fixtures do not validate the stubbed adapters, native provider execution,
statistical calibration or live research data. The canonical integration suite
and platform CI must also pass before adoption.
