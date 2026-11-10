# Loss-aware output digest

`oms runtime output-digest` adds a bounded, metadata-only summary beside an
existing saved tool-output log. It never rewrites raw output, substitutes verifier
input, executes a command, starts a model or changes a task verdict.

```bash
oms runtime output-digest logs/verify.log --format text
oms runtime output-digest logs/verify.log --window head --read-bytes 1048576
oms runtime output-digest logs/verify.log --reported-exit 1 --limit 20 --pretty
```

This is different from `oms job-digest`: that existing command is for training /
Slurm summaries with readable excerpts and optional scheduler queries. This
command has no scheduler or waiting mode, outputs no source lines, and is suitable
for command-output accounting without forwarding log contents into agent context.

## Preserved information and deliberate unknowns

The report gives the observed file size, exact byte window read, unread byte
count, SHA-256 of those raw bytes, partial-line bytes and skipped oversized lines.
A full-file hash is supplied only when the entire observed file was read.

Complete scanned lines matching known traceback/error/test-failure/OOM/nonfinite/
signal patterns are reported as **candidate locators**: byte start/end, line hash,
classes and window-relative line number. A file line number is available only
for a scan from byte zero. One line may have multiple classes. Counts cover the
scanned complete lines even when the displayed candidate list is capped.

No text excerpts are emitted. The output is not a secret-redaction guarantee:
file sizes, hashes and failure classes are still metadata and should remain
private. No automatic outbound export is added.

Matches can include quoted or historical errors, and valid failures can lack
these words. Therefore `diagnosis_complete` is always false and
`process_success` is always null. `--reported-exit` preserves an explicitly
supplied integer (-255..255) as `caller-supplied-unverified`; it is not a receipt.
A successful query with a nonzero reported process exit still returns exit 0.
The absence of matches is never success, nor proof that a gate passed.

## Bounds and safety

The default read cap is 1 MiB; `--read-bytes` accepts 512 bytes through 8 MiB.
Default window is tail; head is explicit. With a tail starting after byte zero,
the first segment is conservatively skipped because the preceding byte is not
read. An unterminated final segment is not classified. Complete lines over 64 KiB
are skipped and counted. At most 100,000 complete lines are classified per query;
remaining lines are counted as unscanned. Candidate output is limited to 1–100
rows (default 20). These conditions make `scan_complete` false; even a complete
scan does not make the heuristic diagnosis complete.

Only the selected repo-local, own-UID, single-link regular file is opened.
Descriptor-relative reads reject symlink path components, traversal, external
paths, special files and changes detected during the read. UTF-8 is required for
classified complete lines; binary NUL or invalid UTF-8 is an error. Raw bytes,
including ANSI escapes, are retained for hashes; bounded ANSI sequences are
removed only for pattern matching. The reader does not persist results or retry
changing logs automatically. Filesystem metadata checks are consistency checks,
not authentication against hostile same-UID processes.

JSON is the default. `--pretty` is JSON-only and cannot accompany text output.
Text rendering reports caps and uses only generated labels/counts/locators.
Exit 2 means invalid/unsafe/unreadable input; runtime Ctrl-C returns 130.
A harness child may use the same read-only query. No existing command behavior,
permission, install default or provider route changes.

## Verification and rollout

`tests/test_output_digest.py` exercises the actual parser and file reader on
synthetic logs and temporary files: exact hashes/byte locators, Unicode/ANSI,
multiple classes, line/read/output caps, missing exit, nonzero exit, malformed
UTF-8, links/FIFO/traversal, concurrent change, no content emission and raw-file
invariance. `--cli` adds actual runtime dispatch/text/JSON/child/invalid-flag
checks. It is registered in the existing `context-core` suite, not a new CI lane.

This implementation does not replace arbitrary provider Bash output, capture a
new log automatically, or create a compacted authoritative artifact. A future
verifier UI can refer to this metadata and the unchanged original artifact.
Rollback is a scoped code revert; there are no persisted records to migrate.
