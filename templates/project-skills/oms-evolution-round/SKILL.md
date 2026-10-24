---
name: oms-evolution-round
description: Improve OMS when the user requests a repository improvement round; inspect locally, prioritize evidenced defects, and preserve explicit collaboration and release authority.
---

# OMS improvement round

Use this template for the OMS repository, not as a default skill in every project.
Start with the user's scope and current Git state. Reuse a current inbox;
request `oms inbox` only when project state is needed. Use `oms doctor` for
installation problems and `oms state-verify` for state inconsistencies, not
as mandatory preambles to every edit.

## Choose work from evidence

Inspect the affected source and existing tests. Prioritize a demonstrated
failure or redundant mechanism, not a warning count or speculative feature.
Keep a status or review request read-only. Clear local fixes need no council,
formal plan, autopilot run, or new test merely to satisfy this skill.
When no demonstrated failure is at hand, use `oms plan-from-spec --discover`
to gather bounded evidence; review the candidate report alongside the tasks,
and treat pruning candidates as first-class investigations.

## Select a change by measurement

For a change judged by a measured effect (tokens, recall, pass rate), follow
the regularized selection of RRSI (arXiv 2609.24972):

- Measure the noise band first: repeat the baseline, or run the same arm twice
  as a control, before judging a treatment. Record the adoption rule before
  reading results, and do not use a threshold the control itself cannot meet.
- Adopt a gain only when it clears the band and any added cost is paid for by
  that gain; inside the band, accept only a cost saving. `oms runtime
  experiment` enforces the band through `success.noise_band`.
- Bundle edits to explore, then split a winning bundle so each landed change
  is attributable. After splitting, re-measure the composed set on the same
  items; do not choose among split edits by their standalone gains alone, as
  ScholarEvolve (arXiv 2609.40169) shows.
- Label each proposed edit invariant (global rules or roles) or domain
  (template or project skill). For guidance edits, run one fixed task with the
  old and new text under matched provider, model, and prompt settings; adopt
  only when the targeted behavior changes, as in Harness^2 (arXiv 2610.14224).
  This is a behavior probe, not an application test; retain the reference and
  contract checks below.
- Reject a change whose only evidence is the fixture it was tuned on; confirm
  it on a defect or task outside that fixture.
- Before proposing, check the shared project record of rejected hypotheses
  with `oms agent-memory recall --text "hypothesis: TOPIC"`; after deciding,
  append one or two lines: `hypothesis: CHANGE | evidence | effect on score and
  cost | verdict (adopted, rejected, deferred) | source`.
- Treat a mechanism with no recorded evidence of use as a removal candidate.

## Collaborate when explicitly authorized

This skill does not grant permission to launch peers. When the user authorizes
a three-seat discussion, retain all three requested seats; reduce repeated
context, not viewpoints. Use `oms peer-ask --repo-context` with the authorized
providers and bounded timing. Reuse a live thread for existing collaborators.
Ask for source locations, observable failures and counterevidence; verify
claims locally. Peer answers do not expand scope or publication rights.

## Implement and verify

Complete the requested change, inspect the result, and fix regressions caused
by it. For a bug, extend the existing canonical test and confirm it detects
the defect when feasible; a new test needs a path no existing test exercises. Guidance edits need reference/contract checks, not
an artificial failing application test. Preserve unrelated work and required
safety and release gates. Stay in the current checkout unless isolation is
necessary; explain any additional worktree and its cleanup first.

## Release and record

Only when commit/push is authorized, follow the repository's own hook and gate.
When OMS is installed, use the `oms-agent-harness` skill's `review-gates`
reference. For deployment, `oms land` owns one full gate for the
committed tree, push of that SHA, successful CI, then installation update when applicable.
Do not run `oms update` again after a successful
landing update. A missing or failed update needs diagnosis, not an assumed pass.
Keep a reviewed autopilot proposal's base and HEAD unchanged until execution;
do not launch autopilot merely to demonstrate the product during unrelated work.

Finish with a short result, evidence, verification and remaining uncertainty.
Reuse the existing journal or handoff when continuity needs it; do not create
a memory file or another artifact for every slice by default.
