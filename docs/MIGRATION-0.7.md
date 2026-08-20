# Migrating to 0.7

Version 0.7 opens the front of the autonomy chain — a vague sentence
becomes a reviewed intent spec — and closes two gaps behind it: a run can
now be re-entered from a fresh session, and a consented landing may
refreeze the definition of done it actually touched. Alongside that, every
verb keeps exactly one spelling. Stored state keeps its shape; one command
surface changes.

## Retired second spellings (the one breaking change)

The dispatcher used to accept two names for five tools. The alias is gone
rather than redirected, so the retired spelling now fails like any unknown
tool. Use the surviving name — the one nearly every caller already used:

| Retired | Use |
| --- | --- |
| `oms repo-state` | `oms state` |
| `oms agent-thread` | `oms thread` |
| `oms runtime-core` | `oms runtime` |
| `oms oms-init` | `oms init` |
| `oms oms-run` | `oms run` |

Scripts under `scripts/` moved with their verbs (`repo-state.sh` →
`state.sh`, and so on); anything calling those paths directly needs the new
filename. Typed data keeps the old labels on purpose: rows already written
with `"tool":"oms-run"` stay readable, and `state-verify`'s `runtime-core`
finding family and `inbox`'s `runtime-core-unavailable` key are consumer
contracts, not command names.

## Project skills carry the oms marker

`oms skill-forge add` now refuses a new project skill whose name lacks the
`oms-` prefix and names the corrected spelling. Skills already stored under
an unprefixed name keep validating, linking, and loading — only the add
gate is strict. The ML template's two project skills ship as
`oms-ml-experiment` and `oms-dataset-safety`; a project that installed them
under the old names keeps those, and re-applying the template will not
install a prefixed duplicate beside them.

The rule is about shared namespaces: forged skills land in `.claude/skills`
and `.agents/skills` next to skills you own, so the marker says which are
the harness's. The `oms` command's own subcommands are already inside that
marker and are never prefixed again.

## New doors (nothing migrates)

- `oms intent draft` turns a goal sentence into a reviewed intent spec and
  `oms intent adopt` is the only writer of `PROJECT.md`. Adopt refuses an
  acceptance command that already passes: a check that passes before the
  work cannot prove the work.
- `oms autopilot reenter` resumes a live run from a fresh session with no
  re-explanation, refusing while any claimant is alive and requeueing the
  leases a provably dead session still held. `oms autopilot abandon` is the
  append-only exit for a contract that cannot finish.
- `oms autopilot shadow` records what re-entry would have decided without
  deciding it. The session-start hook calls it when a live receipt exists;
  it writes only to `.oms/plan/autopilot-shadow.jsonl`.
- `oms autopilot --allow-verifier-change` binds verifier-change consent
  into the run contract, and a consented landing refreezes exactly the
  acceptance-manifest entries its own patch touched.

## Korean output, if you want it

An installed `oms-korean` output style and `OMS_ANSWER_LANGUAGE=ko` apply
clear-Korean rules to final human-facing answers only — never to thinking,
commits, code, or identifiers. Both are opt-in: unset, nothing changes.
