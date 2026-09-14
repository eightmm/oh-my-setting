---
name: oms-trace
description: >
  Diagnose regressions, wrong results or unexplained failures using competing
  hypotheses and discriminating probes; direct known-cause fixes need no worksheet.
---

# Trace

Diagnose before editing; evidence must distinguish cause from coincidence.

For uncertain causes, distinguish:

- **Observation** — what was seen, quoted, not paraphrased.
- **Hypotheses** — plausible alternatives, not an invented quota.
- **Evidence for / against** — per hypothesis, including what is missing.
- **Critical unknown** — the fact preventing a supported conclusion.
- **Discriminating probe** — the cheapest step whose result differs between
  them. Run that one, not the most convenient one.

When direct evidence already isolates the cause, report it and its confirming
check briefly; no hypothesis worksheet or peer call is required.

Evidence is ranked, never flat:

1. A controlled reproduction.
2. A primary artifact with provenance: a log line, a metric, `file:line`, a
   commit, a timestamp.
3. Independent sources converging.
4. A single inference from reading code.
5. Timing, naming, resemblance to a past bug, intuition.

Try to refute your own favourite before adopting it, and down-rank any
hypothesis resting on 4–5 while stronger evidence contradicts it.

Use the failure ledger locally; the peer tools below require explicit
consultation or delegation authority:

- `oms fail-ledger check --cmd "…"` — has this already failed here? Exit 3 means
  yes, with the prior context.
- `oms consult "…"` — one peer when a material uncertainty remains;
  `oms consult --all "…"` only when several independent views are needed.
- `oms advise --prompt "…"` — before an irreversible conclusion or a release
  call.

Report the leading explanation with the tier of evidence behind it and say what
is still unknown. Never state a cause no probe has separated.
