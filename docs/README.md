# Documentation map

Choose an OMS guide by task. Follow the linked sections for detailed contracts and recovery rules.

## Start here

| Your task | Guide | What you will find |
|---|---|---|
| Install or update OMS | [Install](INSTALL.md) · [한국어](INSTALL.ko.md) | Host requirements, profiles and maintenance |
| Prepare or distribute an update | [Native update distribution](UPDATE-DISTRIBUTION.md) | Exact prepared revisions, local bundles and recovery boundaries |
| Find a command | [Components](COMPONENTS.md) | Command catalog, workflows and authority boundaries |
| Choose the right feature scope | [Feature scope](FEATURE-SCOPE.md) | Current roles/authority, optional implementations, excluded expansion and adoption criteria |
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
| [GitHub Workflow (Wiki)](https://github.com/eightmm/oh-my-setting/wiki/GitHub-Workflow) | From an issue and OMS plan to a verified PR and merge evidence |
| [OMS runtime](OMS-RUNTIME.md) | Typed contracts, backends, evidence and adapters |
| [Graph engineering](GRAPH-ENGINEERING.md) | Schemas, lifecycle, validation and CLI |
| [Graph retrieval comparison](GRAPH-RETRIEVAL-COMPARISON.md) | Current Rust/search gaps, five reference projects, reusable implementation packets and retrieval evaluation |
| [Control-plane evolution](CONTROL-PLANE-EVOLUTION.md) | Proposed panel/observation work, acceptance criteria and implementation slices |
| [Control surface and plugins roadmap](CONTROL-SURFACE-ROADMAP.md) | One-repository delivery plan for shared UI, provider session adapters, host plugins and GitHub Project tracking over existing owners |
| [MCP host compatibility](MCP-HOST-COMPATIBILITY.md) | Official Codex/ChatGPT, Claude and Antigravity UI paths, host-specific limits and separate document/fixture/live evidence |
| [Multi-agent and multi-host plan](MULTI-AGENT-MULTI-HOST-PLAN.md) | Proposed behavior, staged delivery and acceptance for parallel agents, Git integration and remote clusters and opt-in Stages A–F runtime implementation |
| [Context inspection](CONTEXT-INSPECTION.md) | Saved context manifest inspection and digest verification |
| [Pinned source intake](INTENT-SOURCE-INTAKE.md) | SHA-pinned local file and GitHub issue intake for `oms intent draft` |
| [Run input identity](RUN-INPUT-IDENTITY.md) | Pre-command input identity and metric contract capture |
| [Work inspection](WORK-INSPECTION.md) | Evidence matrix, next-step explanation and return briefs |
| [Callable consolidation audit](CALLABLE-CONSOLIDATION-AUDIT.md) | Actual CLI/MCP/core wiring, duplication and boundaries to preserve |
| [Consolidation work packets](CALLABLE-CONSOLIDATION-WORK-PACKETS.md) | Ten scoped follow-ups with reusable APIs, callers and canonical fixtures |
| [Tool context review](TOOL-CONTEXT-REVIEW.md) | November 19 source review, implemented CC-04 output reader and native discovery boundaries |
| [Control-plane references](CONTROL-PLANE-REFERENCES.md) | Reference projects, adoption boundaries and evidence requirements |
| [Harness gap ranking](HARNESS-GAP-RANKING.md) | Ranked candidate gaps against other harnesses, orchestrators and collaboration tools, with their relation to existing scope decisions |
| [Research follow-ups](RESEARCH-FOLLOWUPS.md) | Design-only contracts for bounded ACP supervision, live observations and paired context evaluation |
| [Agent harness work packets](AGENT-HARNESS-WORK-PACKETS.md) | Design-only AH-01–13: capability tiers, session adapter contract, conformance tests, guard and sandbox coverage, council seats and collaboration surfaces for arbitrary agents |
| [Memory consolidation](MEMORY-CONSOLIDATION.md) | Proposed incremental Dreaming, evidence-linked recovery lessons, memory validity and existing-owner implementation packets |
| [UI/UX and feature design](CONTROL-PLANE-UX-AND-FEATURES.md) | Comparative UX patterns, task flows, feature proposals and measurable acceptance |

## Overall Main and provider orchestration

[Current implementation and reuse audit](ORCHESTRATOR-COMPLETION-AUDIT.md)
maps the November 18 source to existing PRs, native provider sessions, managed
Team contracts and the remaining implementation work. Read this before creating
another Team implementation plan.

[Lifecycle implementation packet](ORCHESTRATOR-LIFECYCLE-IMPLEMENTATION.md)
specifies the missing native consumers for managed Team/write-WIP retirement,
active cancellation and held-domain recovery, with reusable producers and fixtures.

[Implementation work queue](ORCHESTRATOR-WORK-QUEUE.md) is the current scheduling
index for 40 follow-up IDs from PRs #260–#265: 16 retained, nine absorbed into
existing owner work, and 15 conditional. LC-01–04 remain separate core lifecycle
work. These IDs are specifications and evidence references, not 40 mandatory
projects. The queue records activation conditions, shared-file ownership and the
implementation handoff; older CP/TM/MR/UX/NF documents retain acceptance detail.
The [memory consolidation follow-ups](MEMORY-CONSOLIDATION.md) add DR-01–06:
incremental input preservation and semantic distill first, recovery and validity
next, with conditional wiki work and observation absorbed into existing RF owners.
The [graph retrieval comparison](GRAPH-RETRIEVAL-COMPARISON.md) supplies the
retrieval-specific evidence; small correctness fixes use their nearest fixtures,
while broader ranking/semantic evaluation needs a concrete decision.

[Model upgrade adaptation](MODEL-UPGRADE-ADAPTATION.md) maps current GPT/Codex,
Claude Code and Antigravity guidance to bounded compatibility fixes and existing
MR/TM/REL owners. It distinguishes implemented prompt-audit coverage from remaining
capability, observation, research-permission and subscription-bridge work.

[Bounded team orchestration](TEAM-ORCHESTRATION-PLAN.md) retains the original
architecture and ten implementation slices. Some contract/admission/projection
slices are now implemented; interactive native provider sessions remain a separate
unmanaged path. Neither code presence nor a pane proves complete native child
control, usage coverage or multi-member acceptance.

The [panel layout and interaction contract](TEAM-ORCHESTRATION-PANEL.md) connects
Main-owned direct work, optional Provider Leads and the native session switcher.
It separates the current Auto grouping correction from the proposed task-first
assignment/result inspector and links that work to the existing implementation queue.

## Migration notes

- [MIGRATION.md](MIGRATION.md) — upgrade notes for 0.7 through 0.4, newest first.
