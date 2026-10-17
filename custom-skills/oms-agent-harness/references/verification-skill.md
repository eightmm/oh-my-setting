# Project verification skills

Adapted from pstack by Lauren Tan (MIT).

A project verification skill is an ordinary project skill stored with
`oms skill-forge add`. Give it a prefixed name such as `oms-verify-app` and a
frontmatter `metadata.verify` contract when `oms skill-forge contracts` should
list its declared check. See `scripts/skill-forge.sh` for the accepted format.

## Write the skill

- Inspect source and project docs to identify the user-facing surface and the
  real launch command, readiness signal, and prerequisites.
- Drive one real user path through the product. Use its normal interface and
  stable controls, not internal setters or test-only endpoints.
- Capture the action and resulting state, including visible output and relevant
  side effects. Keep evidence outside scratch state so cleanup preserves it.
- Isolate each instance with its own data, port, or profile where possible.
  Launch only what the check owns, and clean up its processes and scratch data.
- Record the exact commands, expected observations, evidence location, and
  cleanup steps. Keep helper scripts executable and document their invocation.
- Run the instructions once before treating the skill as verified. Confirm
  that cleanup leaves the evidence in place.

## Keep the feature map accurate

List user-facing features from routes, commands, menus, and docs. For each
feature, link its source entry point, user path, drive procedure, and observable
success state. Recheck those links against current source when the product
changes; remove dead paths and add newly exposed features.

## Classify failures

- **Doc drift:** product behavior works, but the skill or map describes it
  incorrectly. Correct the documentation.
- **Harness gap:** the behavior works, but the skill cannot drive or observe it.
  Fix the verification procedure and rerun the path.
- **Product failure:** the documented user path is broken in the product.
  Record the evidence for a product fix; do not rewrite the map to hide it.

Keep evidence and failure notes factual. A successful check covers only the
path it drove and the state it observed.
