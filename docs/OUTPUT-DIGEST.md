# Loss-aware output metadata

`oms runtime output-digest LOG [--exit-code N] [--limit N] --pretty` reads an
explicit repo-local log. It does not wrap or rerun a command, change verifier
input, discard the original log, or write a derived store. It returns no raw log
text, prompt, command arguments, or detected secret values.

The report includes the raw reference, byte coverage, full SHA-256 only when the
whole file was read, hashes/offsets of sampled ranges otherwise, and heuristic
error/timeout/traceback/out-of-memory/non-finite-word counts and byte offsets.
These are word matches, not a reliable classification of failures. The optional
exit code is explicitly caller-supplied and unauthenticated. A query exit of 0
means only that the report was produced, never that the original run passed.

Read budget is 4 MiB: large files use a non-overlapping head/tail split. At most
65,536 line units are scanned, and 8,192 bytes per long line are classified.
Every omitted/unclassified range is counted. Partial boundary lines, invalid
UTF-8 or a cut through UTF-8 are labelled; no sanitized text is emitted. Signal
rows retain the first and last matches within `--limit` (1–100), with total and
omitted counts. A reported absence of matches is never proof of success.

The shared inspector rejects symlinks, hardlinks, special files, traversal and
detected concurrent mutation. Only explicitly selected owned files are read.
There is no implicit scan of provider homes, logs or credentials, and no model
call or token/billing saving claim. This first version is a metadata digest, not
the planned model-facing output replacement/summary or a secret scrubber.

Tests join the existing runtime-core discovery lane and use synthetic logs and
real temporary files. Full CLI/host integration still needs the repository's
canonical tests. Rollback removes the command; source logs are unchanged.
