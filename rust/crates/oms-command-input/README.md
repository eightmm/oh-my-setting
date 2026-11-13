# Native input attachments

Private handler ABI: `owns(args)` classifies without side effects;
`run(args, default_repo, interrupted)` returns `(exit, stdout, stderr)`.
Arguments follow the public `oms input` prefix. Unowned actions return 75 and
empty output before accessing environment or state.

Implemented parent modes are `attach file PATH` (inclusive `--start-line` and
`--end-line`), `attach selection --source browser|chat|text --origin ORIGIN`
(with exactly one of `--text` or `--text-file`), `attachment ID [--full]`, and
`attachments`. They accept `--repo`, `--json`, and leaf help. Top-level and
`attach` help remain unowned until the whole input command is ported. Leaf help,
argument grammar, and diagnostics follow the canonical argparse tree; bounded
fixtures compare 85 captured help/error results byte for byte. Output retains schema 1,
source/content SHA-256 values, provenance, source status, and bounded excerpts.
File sources are limited to 4 MiB; captured UTF-8 snapshots to 64 KiB; default
retrieval to the first complete UTF-8 characters within 8 KiB. Existing feedback
snapshots remain readable up to their canonical 1 MiB bound. Listing counts all
entries against the 1,000-entry limit.

Reads reject symbolic links, multiply linked files, changed files, and files
owned by another user. Existing shared `.oms` permissions and ownership are
preserved. Newly created shared ancestors and private input directories use mode
0700; snapshot files use 0600.
Each capture reserves a fresh random directory. Content is synced before metadata
is published atomically as the completion marker. Concurrent calls create
independent IDs. Handled write failures and interruption remove that capture's
files; listing skips benign stray files and incomplete directories, including
ones removed concurrently. Committed malformed metadata still fails closed. A
kill before metadata publication leaves an unlisted incomplete folder,
while a kill after publication leaves a complete snapshot. A kill while creating
the shared ignore file can leave its temporary file. No capture overwrites
another ID. Existing state and unrelated snapshots survive failed calls.

Git queries resolve physical worktrees and registered parent scratch state using
bounded subprocesses with fixed `core.fsmonitor=false` and per-child
`GIT_OPTIONAL_LOCKS` set to zero. Each child owns a process group; deadline,
output overflow, or interruption kills that group and reaps the direct child.
Successful children remain waitable until group cleanup, preventing PID reuse
from redirecting a cleanup signal. Source paths retain raw Unix bytes through surrogate-escaped
JSON. Source text is never executed. Redirected Git repository/index environments,
tracked private state, and conflicting inherited room repositories are refused.
Capture proves that the exact future content, pending metadata, and committed
metadata paths are ignored before writing sensitive bytes. Existing ignore files
are never replaced. Narrow queue-only ignore rules fail closed. Registry hashes
use the shared `oms-json` Python-compatible canonical encoding, including accepted
float/exponent values; the current view producer emits no floating-point fields.

Child reads currently fail closed **before reading attachments**. Enabling them
requires the native equivalent of `queue.authorize_child_attachment`, including
reciprocal sealed room lineage, the live child enrollment, current sender,
recipient and owner identity signatures, immutable attachment references, and an
exact historical message receipt. Membership or arbitrary IDs in a payload do
not suffice. The owner must connect that verifier before retiring the legacy
child path. Terminal capture and queue actions remain unowned. Public dispatcher,
workspace membership/lock, sole binary tests, and legacy retirement belong to the
parent integration and are outside this package.

Native integration fixtures invoke production `run` with complete argv in
subprocesses so environment and cwd remain unchanged. They use only temporary
Git repositories and private files, with no provider calls or room sends.

Boundary evidence: source bytes cross the explicit path/text boundary; traversal,
link substitution, oversized reads, and unsafe content are controlled by anchored
reads, file identity checks, byte bounds, and content validation. Snapshot bytes
cross the worker retrieval boundary only after authorization; this package denies
that boundary until native identity/receipt proof is available. Integration tests
exercise those refusals without exposing captured text in errors. Existing
terminal snapshots can be read, but their live source status remains unavailable
until the dependent terminal migration supplies its read-only identity validator.
