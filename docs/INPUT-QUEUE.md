# Source-aware attachments and editable inputs

`oms input` captures explicitly selected local context and keeps a small editable
queue of instructions for named OMS room participants. The caller chooses the
room, sender, recipient, and `steer` or `next` mode. Delivery uses the existing
`room.send` path with a stable message ID. It never injects terminal keys, opens a
browser, intercepts a chat, or invokes a model CLI.

The entrypoint is `scripts/input.sh`; reports are JSON. `--json` is accepted for
consistency with other OMS verbs. Capture, add, edit, move, cancel, feedback
import, applied delivery, and reconciliation are parent-only. Read-only
queue inspection is also parent-only. A worker may retrieve one explicit
attachment ID only after its admitted room identity has a confirmed delivery of
that attachment through this queue.

## Capture an attachment

```bash
# Whole file, or an exact inclusive range of lines.
oms input attach file src/model.py --repo .
oms input attach file src/model.py --start-line 30 --end-line 65 --repo .

# Supplied selections: OMS does not read the browser or chat itself.
oms input attach selection --source browser --origin 'https://example.org/paper' \
  --text-file selected-paragraph.txt --repo .
oms input attach selection --source chat --origin 'design discussion, turn 14' \
  --text 'The explicitly selected passage.' --repo .

# Read the last requested lines of one explicitly named live tmux pane.
oms input attach terminal --pane %7 --lines 80 --repo .

oms input attachments --repo .
oms input attachment ATTACHMENT_ID --repo .
oms input attachment ATTACHMENT_ID --full --repo .
```

Every attachment has a new immutable ID, a content SHA-256, byte count, capture
time, and source record. Its private content file is written first; its metadata
is the completion marker. An interrupted capture with no metadata does not appear
in the attachment list. Existing snapshots are never overwritten by an edit.

| Source | Recorded provenance | Verification and limits |
| --- | --- | --- |
| File | Physical worktree, relative path, full-file SHA-256 and size, inclusive selected line range | Owned regular non-symlink file, at most 4 MiB to hash; selected UTF-8 content at most 64 KiB. Paths escaping the repository, `.git`, and `.oms` are refused. |
| Browser/chat/text selection | Caller-declared origin and source kind | Explicitly marked `verified: false`. Supplied UTF-8 text at most 64 KiB. No browser or chat interception, no claim that a URL was visited. |
| Terminal | Exact tmux pane ID, pane process ID, physical cwd, capture line bound | The pane must be live and inside the selected repository; identity and cwd are checked before and after capture. At most 200 lines and 64 KiB. Only `display-message` and `capture-pane` are used. |
| Artifact feedback | Exact imported batch digest, view ID, source-data digest, and registry digest | Full canonical feedback batch at most 1 MiB; the adapter validates the registered row identities and source digest. |

Known secret-like content, credential-bearing URL origins, unsupported control
characters, symlink paths, multiply linked inputs, and oversize inputs are
refused. Secret detection is a best-effort filter, not a guarantee that arbitrary
user-supplied content is safe to share. Attachment retrieval is local output;
forward only the context the recipient needs.

Retrieval shows the first 8 KiB of a snapshot by default and explicitly reports
`truncated`. `--full` returns the entire bounded snapshot. The snapshot's content
digest always covers the full content. Retrieval also reports whether the source
is current, declared and unverified, or drifted/unavailable; a historical snapshot
remains inspectable after its source changes.

File delivery checks the entire source file's current digest, including lines
outside the selected range. Any change requires a new attachment. Terminal
delivery checks the recorded pane, process ID, and cwd; subsequent terminal output
does not change an already captured historical snapshot. A selection's declared
origin remains unverified. Feedback delivery rechecks its exact view registry.

## Build and edit the queue

```bash
oms input add --room ROOM_ID --from MAIN --to WORKER --mode steer \
  --text 'Use the selected implementation constraint in the current task.' \
  --attachment ATTACHMENT_ID --repo .

oms input add --room ROOM_ID --from MAIN --to WORKER --mode next \
  --text-file next-task.txt --repo .

oms input list --repo .
oms input show INPUT_ID --repo .
oms input edit INPUT_ID --text 'Updated instruction.' --repo .
oms input edit INPUT_ID --attachment NEW_ATTACHMENT_ID --repo .
oms input edit INPUT_ID --clear-attachments --repo .
oms input move INPUT_ID --before ANOTHER_QUEUED_INPUT --repo .
oms input move INPUT_ID --repo .
oms input cancel INPUT_ID --repo .
```

Only queued inputs can be edited, reordered, or cancelled. Each edit advances the
item's version. Supplying `--attachment` to `edit` replaces its entire attachment
list; it does not mutate any saved attachment. Omitting `--before` moves an item
to the end. Up to eight distinct attachments may be referenced by one input.

The queue records an explicit room lineage, sender, recipient, room owner,
provider/native consumer identities, task/scope metadata, and enrollment epochs.
A leave and rejoin is a new epoch even if a participant reuses the same label or
provider. The target binding is not editable: cancel and add a newly addressed
input when the intended recipient or owner changes. No broadcast or self-addressed
input is allowed.

`steer` means the instruction concerns the current task. It uses ordinary room
delivery at the recipient's next safe read point; it does not claim that a native
model was interrupted. `next` is a following task and requires the caller's
explicit boundary assertion when delivering. There is no background dispatcher,
idle-state inference, scheduler, or automatic model launch.

Order is enforced within each room-lineage/sender/recipient/mode lane. A later
queued item cannot bypass an earlier item in its lane without an explicit move.
Steering one target does not wait for an unrelated target's next-task lane.

## Preview and deliver

```bash
oms input preview INPUT_ID --repo .
oms input deliver INPUT_ID --repo .

oms input deliver STEER_INPUT_ID --apply --repo .
oms input deliver NEXT_INPUT_ID --apply --at-boundary --repo .

# Optional protection against an edit since the reviewed preview.
oms input deliver NEXT_INPUT_ID --apply --at-boundary \
  --expected-sha256 PREVIEW_PAYLOAD_SHA256 --repo .
```

Both `preview` and a delivery without `--apply` validate source drift and current
room ownership, then return the exact message text, byte count, and SHA-256 that
would be sent. They do not send a message. `--at-boundary` is an explicit caller
assertion, not a claim that OMS observed an idle model.

Room messages contain the instruction plus attachment IDs, content digests, source
verification labels, and `oms input attachment ID` retrieval hints. Attachment
contents are not pasted into the room log. The complete composed message must fit
the room's 4,000-byte limit; an oversized instruction is refused rather than
silently truncated. Plain instruction text is capped at 3,500 bytes before adding
the reference and delivery metadata.

An applied delivery follows this sequence:

1. Lock and reload the queue; verify its item is queued and first in its lane.
2. Check the source worktree, all attachment references and current file digests,
   and the exact room/sender/recipient/owner binding.
3. Persist the complete delivery intent, stable message ID, payload digest, and
   `uncertain` status atomically **before** calling `room.send`.
4. Append the message through the existing room path. It retains that path's
   validation, idempotency, lock, and rollover behavior.
5. Find the exact original message in the verified room lineage. Check its text,
   ID, sender, recipient, and identities at the historical append point before
   recording `delivered`.

Source reads, queue state, and the room append are separate stores. This is not a
transaction spanning all three. A concurrent identity change that prevents exact
confirmation leaves the attempt uncertain. The durable intent is evidence of an
attempt, not evidence of successful delivery. A delivered receipt means the
addressed message is persisted in the room; it does not prove the native UI
displayed it, a model acted on it, or anyone approved the work.

Concurrent delivery attempts are serialized by the shared queue lock. A repeated
delivery of an already confirmed item returns its existing result without sending
another message. Use reconciliation for a fresh check of the room evidence.

## Reconcile an uncertain result

```bash
oms input reconcile INPUT_ID --repo .
```

An interrupted process, append error, timeout, or unverified result leaves the
item `uncertain`. Applied delivery and reconciliation return exit code 3 when a
structured result remains uncertain. Invalid arguments, stale sources, and
refused actions return exit code 2.

Reconciliation reads the room's reciprocal, sealed predecessor chain and checks
the exact original message. Carried copies cannot masquerade as additional
deliveries or new enrollment epochs. A matching original confirms delivery even
after rollover. Explicit room acknowledgments are reported separately and do not
grant approval.

If no matching message is observed, the item stays uncertain. If its message ID
appears with different content or addressing, reconciliation fails closed. Neither
case automatically resends the message, resets the item to queued, or permits an
uncertain item to be edited or cancelled. This avoids duplicating a message whose
effect may have happened before the sender lost its receipt.

## Queue artifact-view feedback

```bash
oms input feedback --batch IMPORTED_BATCH_SHA256 \
  --room ROOM_ID --from MAIN --to WORKER --mode next --repo .
```

The adapter reads an already imported batch from
`.oms/artifact-views/feedback.jsonl` and the registry record at
`.oms/artifact-views/VIEW_ID.json`. It independently checks:

- Schema 1 and the exact SHA-256 of canonical `{view_id, data_sha256, events, report}`.
- A matching registry ID and source-data digest, and every event row key's
  membership in the registered `row_keys` list.
- At most 1,000 events and 1 MiB of canonical feedback. Event actions are exactly
  `select`, `accept`, `reject`, `uncertain`, or `comment`; comments are strings of
  at most 4,000 characters.
- Optional report fields `title`, `question`, `decision`, and `next`, each a string
  of at most 12,000 characters. Unknown report fields are refused.

It creates one immutable feedback attachment containing the complete events and
report, then one queued instruction carrying `report.next`, action counts, and
the exact attachment reference. The queued item's provenance retains the batch
digest, view ID, source digest, and every `select`/`accept` row key. Counts do not
replace the full selections: the attachment preserves every row and comment for
retrieval.

A long `report.next` that cannot fit the room budget raises an explicit error;
the adapter does not shorten it silently. The caller can keep the full feedback
as a supplied attachment and write a shorter instruction. Repeating the same batch
returns its original queue mapping. A different recipient identity or mode cannot
reuse that mapping to redirect the batch. Importing feedback does not deliver the
input or start a model. `accept` in a view is feedback data, not OMS patch admission
or publication authority.

## State, scope, and verification

Private state lives under ignored `.oms/inputs` paths in the existing shared state
repository. The existing `room_repository` resolver maps a recognized scratch
worktree to that source repository; attachment metadata still names the actual
execution worktree whose file or terminal was captured. A conflicting inherited
room repository is refused. Workers with `OMS_ROOM_REPO` can retrieve references
from that shared room context without inheriting write authority. A worker also
needs `OMS_ROOM_ID` and `OMS_ROOM_PARTICIPANT`, a live admitted child enrollment,
and a matching confirmed room message addressed to that exact identity. Another
recipient, a different room, or a leave/rejoin cannot inherit those attachment
reads. Listing all attachments or inspecting the queue remains parent-only.
Merely mentioning an attachment ID in an unrelated room message does not grant
access through this interface. Ambient Git repository/index redirect variables
are refused so they cannot reinterpret an explicit `--repo` or its shared-state
bridge.

The active record is `queue.json`, written atomically under the existing OMS lock
protocol. It is bounded to 4 MiB and 256 items, including completed history. No
background retention process deletes history. Attachment directories are private,
metadata/content files are mode 0600, and listing is bounded to 1,000 entries.
Room-lineage reconciliation is capped at 32 MiB. All paths reject symlink traversal
and queue state must remain ignored and untracked.

`python3 -m unittest discover -s tests -p test_inputs.py -v` exercises these new
public entrypoints with real temporary Git repositories, real room sends and
acknowledgments, and a fake tmux limited to read operations. Coverage includes exact
source digests and drift, immutable references, edit/reorder/cancel behavior,
boundary and digest checks, concurrent delivery, uncertainty before and after a
real append, message conflicts, owner/participant changes, rollover receipts,
feedback validation/idempotency, and child/symlink/secret/size boundaries. The
suite invokes no real provider CLI or external messaging service.
