# Skill Lifecycle

Use this reference when improving, importing, evaluating, or deriving a project
skill. `skill-forge` is the only activation authority; do not write links or
`.oms/skill-store` by hand.

## Choose the operation

| Intent | Command | Boundary |
|---|---|---|
| Inspect an external bundle | `oms skill-forge preview --source SOURCE --json` | No project write and no bundle execution. Record the returned bundle SHA. |
| First import | `import --expected-bundle-sha256 SHA --apply` | Content-addressed quarantine plus project discovery links. Requires an `oms-*` name. |
| Update | `update --expected-current-sha256 OLD --expected-bundle-sha256 NEW --apply` | Both current provenance and previewed new bytes are CAS-bound. |
| Roll back | `rollback oms-NAME --to SHA --expected-current-sha256 CURRENT --apply` | Selects an already stored immutable revision. |
| Measure routing/task value | `eval NAME --suite SUITE --allow-host-commands` | Explicit host execution in fresh baseline/treatment dirs; no sandbox claim. Add `--record` only when the aggregate result should enter runtime benchmark telemetry. |
| Learn from prior work | `derive --from thread|journal|attempt-ref --id ID --name oms-NAME` | Preview by default. `--apply` writes an inert review draft, never an active skill. |

Preview before every import/update. Never execute code merely to inspect a
bundle. A bundle with symlinks, hardlinks, nonregular or sensitive entries,
oversized content, embedded credentials, invalid frontmatter, or digest drift
must stay refused. Git provenance records the requested ref and resolved
revision; a local source records a path digest without publishing the path.

An evaluation suite is untrusted input. Every router/task/verify command must
be an explicit argv array, and every host command requires the caller's
`--allow-host-commands`. Compare aggregate trigger confusion counts and task
pass delta; do not use stored prompts or outputs as telemetry.

A derived draft must cite sufficient bounded source evidence. Review and edit
its `SKILL.md` and `REVIEW.md`, then use the normal local `skill-forge add|link`
flow if it deserves activation. Draft creation alone proves neither correctness
nor recurrence.
