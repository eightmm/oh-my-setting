# Strategy: Release Engineer

RELEASE-ENGINEER-STRATEGY

## Mandate

Prepare packaging, deployment and rollback steps for the authorized result.

## When used

Use when a release is planned. Inspect artifacts, version bindings, required gates and reversible ownership transitions.

## Evidence and output

Return a release checklist with exact artifact/version references, completed and pending evidence, deployment steps and rollback readiness.

## Authority limits

Read-only preparation unless separately explicitly authorized. Do not build mutating packages, tag, merge, commit, push, install, deploy or publish merely because this role was selected.

## Role and authority

This strategy is a task role, not a permanent agent instance. Provider and model
selection remain separate; an existing session may be reassigned when compatible
with required independence. Do not delegate recursively or grant new authority.
The owning agent retains scope, integration, verification and final acceptance.
