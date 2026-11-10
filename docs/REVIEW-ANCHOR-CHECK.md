# Exact-diff finding anchor check

```bash
oms runtime evidence anchors findings.json --diff review.diff \
  --expected-diff-sha256 FULL_SHA256 --limit 20 --pretty
```

This is a read-only location checker, not a new review gate or finding store.
The caller explicitly supplies the reviewed diff digest. Both files must be
bounded, owned, non-linked repo-local regular files. No path found in a diff or
finding is opened; current checkout bytes are not read or verified.

Input is `{"schema":1,"findings":[{"id":"optional-id","path":"src.py",
"side":"new","line":12,"quote":"exact bounded single-line quotation"}]}`.
Up to 256 findings are accepted. Missing IDs get a deterministic location/quote
fingerprint; this fingerprint is not a stable cross-revision finding identity.
The checker does not make writers emit this schema automatically. Integrating
structured reviewer output and finding lifecycle remains separate work.

Statuses are `in-diff`, `context`, `outside-observed-hunks` and
`anchor-not-found`. The last two do not mean the finding is false: it may cite
unshown code or a different revision. Quotes are substring-checked against that
exact side and line only. Paths/locations are returned, but source/quote/summary
text is not echoed. Findings are never fixed, verified, dismissed or upgraded to
blocking authority by this tool. The existing review/admission verdict remains
unchanged.

The parser supports ordinary Git unified diffs, additions/deletions, changed
paths, multiple hunks and quoted UTF-8 paths. Malformed/truncated/overlapping
hunks, unsafe names, unsupported combined diffs and excessive ranges fail closed.
Binary changes are explicitly uninspected. The input cap is 4 MiB, 100,000 diff
lines and 100,000 indexed side/line anchors; output lists carry omitted counts.

The unit tests and full-CLI integration are discovered by the existing runtime
suite. No provider call, network request, GitHub comment, state writer or extra
CI lane is added. Rollback requires no data migration.
