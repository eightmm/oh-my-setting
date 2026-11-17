# Documentation map

Choose an OMS guide by task. Follow the linked sections for detailed contracts and recovery rules.

## Start here

| Your task | Guide | What you will find |
|---|---|---|
| Install or update OMS | [Install](INSTALL.md) · [한국어](INSTALL.ko.md) | Host requirements, profiles and maintenance |
| Find a command | [Components](COMPONENTS.md) | Command catalog, workflows and authority boundaries |
| Choose the right feature scope | [Feature scope](FEATURE-SCOPE.md) | Core behavior, optional workflows and adoption criteria |
| Define a project | [Project context](PROJECT-CONTEXT.md) | Purpose, decisions and work evidence |

## Using OMS

| Your task | Guide | Start with |
|---|---|---|
| Run and coordinate agents | [Terminal panel](TERMINAL-PANEL.md) | [Quick start](TERMINAL-PANEL.md#quick-start) · [Keys](TERMINAL-PANEL.md#keys) |
| Review or accept work | [Panel results](TERMINAL-PANEL.md#discussion-and-results) | [Finalize](TERMINAL-PANEL.md#finalize-an-outcome) · [Accept a patch](TERMINAL-PANEL.md#accept-and-publish-a-patch) |
| Recover interrupted work | [Panel recovery](TERMINAL-PANEL.md#salvaging-a-stopped-worker) | [Settle workers](TERMINAL-PANEL.md#settle-stopped-workers) · [Cleanup](COMPONENTS.md#cleanup) |
| Inspect recorded progress | [Dashboard](DASHBOARD.md) | Read-only snapshot and JSON schema |
| Keep work history | [Work Journal](WORK-JOURNAL.md) | Local capture and optional Notion mirror |
| Use another Codex harness | [Codex harnesses](CODEX-HARNESSES.md) | Codex inference through Pi or Claude Code |

## Reference

| Reference | For |
|---|---|
| [Contributing](../CONTRIBUTING.md) | Scoped checks, publication, style and compatibility |
| [OMS runtime](OMS-RUNTIME.md) | Typed contracts, backends, evidence and adapters |
| [Graph engineering](GRAPH-ENGINEERING.md) | Schemas, lifecycle, validation and CLI |
| [Control-plane evolution](CONTROL-PLANE-EVOLUTION.md) | Proposed panel/observation work, acceptance criteria and implementation slices |
| [Multi-agent and multi-host plan](MULTI-AGENT-MULTI-HOST-PLAN.md) | Proposed behavior, staged delivery and acceptance for parallel agents, Git integration and remote clusters and opt-in Stages A–F runtime implementation |
| [Context inspection](CONTEXT-INSPECTION.md) | Saved context manifest inspection and digest verification |
| [Pinned source intake](INTENT-SOURCE-INTAKE.md) | SHA-pinned local file and GitHub issue intake for `oms intent draft` |
| [Run input identity](RUN-INPUT-IDENTITY.md) | Pre-command input identity and metric contract capture |
| [Work inspection](WORK-INSPECTION.md) | Evidence matrix, next-step explanation and return briefs |
| [Control-plane references](CONTROL-PLANE-REFERENCES.md) | Reference projects, adoption boundaries and evidence requirements |
| [Research follow-ups](RESEARCH-FOLLOWUPS.md) | Design-only contracts for bounded ACP supervision, live observations and paired context evaluation |
| [UI/UX and feature design](CONTROL-PLANE-UX-AND-FEATURES.md) | Comparative UX patterns, task flows, feature proposals and measurable acceptance |

## Proposed multi-team execution

[Bounded team orchestration](TEAM-ORCHESTRATION-PLAN.md) defines the proposed
single-Main, OMS-managed/native-team boundary and ten implementation slices.
This is a design and delivery plan, not an enabled runtime feature.

## Migration notes

- [MIGRATION.md](MIGRATION.md) — upgrade notes for 0.7 through 0.4, newest first.
