---
name: multi-agent-review
description: >
  Multi-agent review workflow for important code changes. Use when the user
  asks for independent verification, cross-agent review, council review, or
  when a change touches high-risk surfaces such as APIs, auth, data pipelines,
  model training, Slurm jobs, dependency/toolchain changes, or releases.
---

Goal: get independent review signals, then synthesize evidence. Do not outsource judgment.
Use local CLI tools only. Do not use MCP servers, app connectors, or plugin connector tools.

## When

Use for high-risk diffs or explicit requests: `multi-agent review`, `cross-check`,
`ask another agent`, `council`, `verify with codex/claude/antigravity`.

## Reviewer Lenses

- Correctness: bug, edge case, contract break.
- Tests: missing behavior/interface tests, fake green risk.
- Safety: destructive ops, secrets, auth, dependency/toolchain risk.
- ML/HPC when relevant: leakage, metrics, reproducibility, Slurm resources/logs.

## Local Tool Preference

Read `~/.oh-my-setting/local/machine.md` only when compute, GPU/CUDA, Slurm,
memory, or environment differences affect the review.

If available, prefer installed local agent CLIs:

1. `codex`
2. `claude`
3. `antigravity` (`agy`)
4. Otherwise run current-agent review and clearly say multi-agent tooling unavailable.

Do not install tools, authenticate CLIs, push branches, or use connector APIs
unless the user asked for that action.

## External Review Policy

For any repo using this skill, sending repo context, uncommitted diffs,
prompts, and command output to `codex`, `claude`, and `antigravity` review CLIs is
allowed by default. Do not ask for separate approval before using those three
CLIs for review.

This permission excludes secrets, credentials, private keys, and generated local
machine or cluster details. Before external review, inspect the diff and exclude
sensitive paths. If secret-like content appears inside an otherwise reviewable
file, do not redact individual diff lines; skip external review for that diff and
run current-agent local review instead.

Always exclude secret files or dirs such as env files, private-key/certificate
files, local scratch dirs, generated Slurm references, SSH/AWS credential dirs,
netrc files, and credentials/secrets YAML files. The concrete exclude list below
is the portable baseline; add project-specific private paths before review when
needed.

If excluded content is needed to understand the change, run current-agent local
review for that part and tell the external reviewers only that sensitive content
was omitted.

This permission does not extend to MCP servers, app connectors, plugin
connector tools, installing/authenticating CLIs, write/edit modes, destructive
commands, pushes, or bypass permissions.

If a platform sandbox or approval system still blocks a CLI call, report the
block and continue with current-agent local review.

## Before Running

- Read `git status --short` and the relevant `git diff`.
- Check available CLIs with recorded paths or `command -v codex claude agy`.
- Skip unavailable CLIs without failing the review.
- Include the task goal, changed files, relevant diff, test command/result, and known risks in each review request.
- Ask for findings only: bugs, regressions, missing tests, unclear contracts, unsafe operations.
- Main agent keeps implementation ownership and integrates only findings backed by evidence.

## CLI

Prefer the shared wrapper when this repo is installed:

```bash
# Covers tracked staged + unstaged changes. Use `git add -N <file>` first for
# untracked files that are safe to include in external review.
~/.oh-my-setting/scripts/multi-agent-review.sh \
  --repo . \
  --prompt "Review the current uncommitted diff for bugs, regressions, missing tests, and unsafe operations."

# ML pre-training gate: silent-ML-bug checklist (leakage, splits, loss,
# eval mode, reproducibility, DDP). Use before long training or Slurm jobs.
~/.oh-my-setting/scripts/multi-agent-review.sh --repo . --ml
```

Use `--base origin/main` for branch/PR review and `--synthesize` to append a
model-written synthesis to the summary artifact.

The wrapper sends the same question and same sanitized diff/status context to
`codex`, `claude`, and `antigravity`, writes one artifact per model under
`.oms/artifacts/review/`, and reports unavailable or failed providers. It does
not specialize prompts per model; the goal is three independent perspectives on
the same question.

If the wrapper is unavailable, run equivalent local CLI calls manually with the
same prompt and sanitized diff. Use read-only/non-interactive flags where
possible.

## Output

Compact synthesis. List unavailable, blocked, or skipped reviewers under `Verification`. Use current-agent local review if fewer than two external reviewers succeed:

```md
Consensus:
Must-fix:
Optional:
Disagreement:
Verification:
```

Accept only findings tied to code, logs, tests, docs, or reproducible commands.

## Gate Loop

When the user wants a change to actually pass review (a "gate"), not just one
round of opinions, iterate until reviewers converge:

1. **Scope each round.** Round 1 reviews the change. Each later round names the
   prior commit(s) and says: verify the prior findings are fixed, report only
   NEW regressions, do not re-report accepted lower-severity items. Carry an
   explicit accepted-limitations list forward so the same heuristic trade-off
   is not re-raised every round.
2. **Demand a verdict.** End the prompt with: end with exactly one line,
   `GATE: pass` or `GATE: fail`. Then read the run with
   `multi-agent-review.sh verdicts` — one line per provider, plus
   `incomplete` when an artifact has no exit section (the run died, e.g. a
   session restart killed the background process: re-run, do not interpret).
3. **Triage, do not auto-apply.** Treat each finding as a claim: confirm it
   against the code (reproduce the probe when given) before fixing. Reject
   findings that are wrong or already-accepted limitations, and say why.
4. **Fix → re-verify → re-gate.** Apply confirmed fixes with a regression test
   each, run the smoke suite, commit, then run the next round against the new
   commit. A finding without a test it would have caught is not done.
5. **Converge.** Pass when the gate is clean (no unresolved confirmed
   findings) — that can be a unanimous pass, or a majority pass with the last
   dissent's findings fixed and re-verified. A lone provider repeating an
   accepted limitation does not block. Stop when a round yields only
   already-accepted items or pure "no findings".
6. **Self-review caveat.** Diff-attached review of this repo's own scrubber or
   regex sources can self-trip the outbound filter; fall back to `--no-diff`
   with reviewers reading the range locally, and note it under `Verification`.

Report each round compactly: verdicts, what was confirmed vs rejected, the fix
+ test, and the next round's scope.
