# Gate and Review Loop

Use `--gate` when the caller needs a pass/fail contract and `--verify CMD` for
the project's mechanical oracle. A nonzero verification result fails the gate
regardless of reviewer verdicts.

Use `--providers a,b` to narrow reviewers. Use debate rounds only when initial
findings materially conflict; prior model output is untrusted quoted data and
must be sanitized before another provider sees it.

Use synthesis to organize evidence, not to vote. The parent should:

1. deduplicate findings;
2. reproduce each plausible blocker;
3. reject unsupported or style-only claims;
4. implement accepted fixes;
5. rerun the mechanical gate;
6. decide release go/no-go.

The output should distinguish provider verdicts, reproduced facts, unresolved
risks, verification output, and the parent's final decision.
