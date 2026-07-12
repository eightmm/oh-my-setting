# Design Review

Stop and flag:

- hypothesis written after results (HARKing);
- metric or threshold changed after inspection;
- best seed/checkpoint reported without spread;
- no baseline or ablation;
- several variables changed together;
- confirmation-only search with no disconfirming run;
- leakage-flattered metrics;
- post-hoc interpretation that ignores the prior prediction.

Keep negative and null results: they prune the search space and prevent repeated
dead ends.

For an explicitly requested council or release gate, run:

```bash
oms peer-ask --hypothesis --prompt "<hypothesis and planned experiment>"
```

Independent reviewers should attack falsifiability, confounds, leakage,
baseline strength, metric variance, compute cost, and the decision rule. A
routine experiment does not require three providers.
