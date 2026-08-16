# Migrating to 0.5

Version 0.5 integrates the Typed Runtime Core directly into the product and
makes installation capability-scoped. Nothing migrates automatically; every
change below is opt-in or backward compatible.

## Capability profiles

A fresh 0.5 install defaults to the `core` capability profile: Bash, Git,
Python, and exactly one coding-agent provider, with no service logins. A
private capability receipt records the selection.

- **Existing installs change nothing.** An install with no capability receipt
  keeps the legacy full-tool update path byte for byte.
- **Opting in** is one explicit apply — `oms install-profile --apply
  --profile core --profile github` (choose your own set). From then on
  `oms update` refreshes exactly that selection.
- **Rolling back** is deleting the receipt
  (`~/.config/oh-my-setting/capabilities.json`); the next update restores
  legacy full-tool behavior. Uninstall removes the receipt with the install.
- `--full` remains the explicit compatibility install with the historical
  all-tools footprint.
- With a receipt present, doctor names each absence precisely: a selected
  capability's missing tool is a defect; an unselected capability reads as
  "not installed" with the exact add command. An installed CLI is always
  usable regardless of selection.

## Typed runtime projection

`oms runtime` (envelope, evidence, context, profile, release, doctor,
capsule, experiment, failure) works directly on the checkout. `oms state`,
`oms inbox`, and state-verify read the runtime projection; a broken
projection is reported fail-closed, never guessed around. Runtime state is
projection or evidence only — it grants no mutation, landing, commit, push,
or publication authority.

## Update channels

`config/update-channels.json` is an advisory projection surface read from the
checkout's own copy (`oms runtime release status`); `scripts/update.sh` never
consults it and `auto_apply` stays false. The `stable` channel names the
release commit by exact SHA; the promotion that records a new stable lands in
a follow-up commit, so a checkout sitting exactly on a release commit reads
the previous stable in its own manifest — one commit of skew by design, and
advisory either way.

## Failure taxonomy

Fail-ledger rows now carry a canonical `failure_code` and `recovery` beside
the free-form reason, and `fail-ledger check` surfaces them in every refusal
built on it. Rows written before 0.5 lack the fields and stay readable.
