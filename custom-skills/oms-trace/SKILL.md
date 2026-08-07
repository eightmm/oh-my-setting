---
name: oms-trace
description: >
  Explain why an observed failure or anomaly happened before changing anything:
  competing hypotheses, ranked evidence, and the cheapest probe that tells them
  apart. Use for regressions, silent wrong results, flaky or unreproducible
  behaviour, and "why did this happen" questions.
---

# Trace

A fix aimed at an undiscriminated hypothesis changes code without changing the
cause, and the evidence that would have separated them is gone once the code
moves. Explain first.

Keep these apart, in writing:

- **Observation** — what was seen, quoted, not paraphrased.
- **Hypotheses** — at least two that compete.
- **Evidence for / against** — per hypothesis, including what is missing.
- **Critical unknown** — the fact keeping the leading two apart.
- **Discriminating probe** — the cheapest step whose result differs between
  them. Run that one, not the most convenient one.

Evidence is ranked, never flat:

1. A controlled reproduction.
2. A primary artifact with provenance: a log line, a metric, `file:line`, a
   commit, a timestamp.
3. Independent sources converging.
4. A single inference from reading code.
5. Timing, naming, resemblance to a past bug, intuition.

Try to refute your own favourite before adopting it, and down-rank any
hypothesis resting on 4–5 while stronger evidence contradicts it.

Reach for:

- `oms fail-ledger check --cmd "…"` — has this already failed here? Exit 3 means
  yes, with the prior context.
- `oms consult --all "…"` — independent hypotheses; distinct model families
  fail differently, and the run reports how many actually answered.
- `oms advise --prompt "…"` — before an irreversible conclusion or a release
  call.

Report the leading explanation with the tier of evidence behind it and say what
is still unknown. Never state a cause no probe has separated.
