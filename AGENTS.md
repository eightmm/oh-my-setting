# Global Coding Guidelines

These rules apply by default when writing, reviewing, or refactoring code.

- Think before coding. State assumptions explicitly, surface ambiguity, and ask when the next step would otherwise depend on guessing.
- Prefer the simplest implementation that solves the requested problem. Do not add speculative features, configurability, or abstractions.
- Make surgical changes. Touch only files and lines that directly support the task, match the existing style, and avoid unrelated cleanup.
- Preserve user or existing changes. Do not revert or rewrite unrelated work; work with it when it affects the task.
- Define verifiable success criteria for non-trivial changes. Prefer a short plan that pairs each step with a check.
- Verify the work. Run the narrowest useful tests or commands, and report any checks that could not be run.
- Keep explanations direct. Mention tradeoffs, risks, and remaining uncertainty instead of hiding them behind confident language.

