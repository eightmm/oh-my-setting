# Native session exploration

`oms session` connects a chosen local Claude, Codex, or Pi conversation to an
independent copy of the current Git worktree. It also searches local conversation
text, keeps exact bookmarks, compares fork files and artifact hashes, and exposes
file restore and conversation restore as separate actions.

The entrypoint is `scripts/session.sh`. Reports are JSON; `--json` is accepted for
consistency with other OMS commands. Native interactive launches inherit the
terminal, so an applied launch can print native UI output before its final report.
No action embeds a transcript, uploads one, rewrites native JSONL, or silently
submits a model prompt. `--apply` is required to launch a native CLI or restore
files. Session exploration is parent-only, including transcript reads.
An OMS-enrolled Claude or Codex session resumes through the existing panel native
owner path, retaining its room-binding and live-owner checks. Ambiguous ownership
or managed Pi ownership is refused here; the existing room chat navigation remains
the route to an already running terminal. Standalone native sessions use the
capability-checked native command directly.
Standalone applied conversation actions hold a provider/session claim for the
duration of the native process. A concurrent OMS caller uses a one-second lock
timeout and refuses the launch if the claim stays busy; another resume is allowed after the
first process exits. This coordinates OMS callers only. It does not establish
ownership of a provider process launched separately outside OMS. Managed panel
sessions keep their existing live-owner and room-binding checks.

## Find and reopen a conversation

```bash
oms session search 'the exact phrase' --repo .
oms session search 'chosen design' --provider codex --repo .
oms session open --provider codex --session NATIVE_ID --line 15 --limit 10 --repo .
oms session resume --provider codex --session NATIVE_ID --repo .
oms session resume --provider codex --session NATIVE_ID --repo . --apply
```

Search is literal and case-sensitive. It returns only user and assistant text
parts, excluding tool results, reasoning blocks, attachments, and raw request
payloads. A result identifies the provider, exact native session ID, local file,
JSONL line, part index, JSON pointer, and original character offsets. Excerpts
keep the original characters. Secret-like excerpts are replaced with an explicit
withheld marker; offsets still describe the original text. This is a best-effort
display filter, not proof that arbitrary conversation text is safe to publish.
Search results are local and should not be forwarded wholesale to another model
or shared room.

Default discovery visits only the provider's known session directories:

| Provider | Native directory | Repository identity |
| --- | --- | --- |
| Claude | `$CLAUDE_CONFIG_DIR/projects`, otherwise `~/.claude/projects` | `sessionId` and `cwd` in the first bounded metadata/message rows |
| Codex | `$CODEX_HOME/sessions` and `archived_sessions`; `$OMS_CODEX_HOME` is the fallback before `~/.codex` | `session_meta.payload.id` and `cwd` |
| Pi | `$PI_CODING_AGENT_SESSION_DIR`, otherwise `$PI_CODING_AGENT_DIR/sessions`, otherwise `~/.pi/agent/sessions` | Native `session` header `id` and `cwd` |

The native `cwd` must resolve to the selected Git repository root. Filenames,
project slugs, and text mentioning a repository do not establish that binding.
Sessions started in a subdirectory or a different worktree are intentionally not
included in that root's results. A session without recognized metadata is not
launchable through this command. Use the native UI for unsupported formats.

Narrow or override discovery with explicit directories. Cross-provider roots
carry a provider prefix:

```bash
oms session search 'design choice' --repo . --provider claude --root ./saved-sessions
oms session search 'design choice' --repo . \
  --root claude=./claude-sessions --root pi=./pi-sessions
```

Discovery never scans the whole home directory. It refuses HOME and filesystem
roots as explicit scopes, does not follow symlinks, and reads only owned regular
files with one link. Duplicate native IDs in the selected scope are ambiguous;
opening or launching requires a narrower root rather than picking one silently.
Custom roots outside the configured Claude/Codex stores support search, opening
and bookmarks only. ID-based native launches require the source to be inside
that provider's configured store, so a copied transcript cannot silently resolve
the same ID in a different account home. Pi uses the verified exact file path.

The bounds are 8,192 visited entries, 4,096 candidate files, 64 KiB of metadata
per file, 32 MiB per transcript, 1 MiB per message line, and 64 MiB of transcript
content per search. The default result limit is 20, with a maximum of 100.
Discovery, skipped-file, byte, and truncation counts are in the report. Oversized
or malformed lines are not searched. This is a bounded local view, not a claim
that all historical conversations have been searched.

## Bookmark an exact origin

```bash
oms session bookmark add --provider claude --session NATIVE_ID \
  --line 25 --part 0 --label 'alternative considered' --repo .
oms session bookmark list --repo .
oms session bookmark open BOOKMARK_ID --repo .
oms session bookmark resume BOOKMARK_ID --repo . --apply
```

A bookmark stores a native file reference, line and part, and the line's digest;
it does not copy the message text. Reopening rereads the original file and checks
its provider, session, repository, and line digest. Appending later turns does
not invalidate an earlier bookmark. Replacing the bookmarked line makes the
bookmark stale. `bookmark resume` resumes the native conversation; it does not
claim that the native CLI can resume at that specific line.

## Fork conversation and files

```bash
oms session fork --provider codex --session NATIVE_ID --name 'approach B' --repo .
oms session fork --provider codex --session NATIVE_ID --name 'approach B' --repo . --apply

# Prepare the independent checkout now and launch the conversation explicitly later.
oms session fork --provider claude --session NATIVE_ID --repo . --apply --no-launch
oms session launch FORK_ID --repo .
oms session launch FORK_ID --repo . --apply
oms session forks --repo .
```

The dry run checks installed native capabilities and reports what file state
would be copied. Applying first creates an ordinary OMS checkpoint, then creates
a detached scratch worktree at that checkpoint's HEAD using
`scratch-worktree.sh`. It applies the saved index patch with `git apply --index`,
then the unstaged patch, and finally the bounded untracked archive. Staging stays
staging. Source files, source HEAD, and the source native transcript are not
changed by OMS's fork preparation.

The untracked capture uses the existing checkpoint limits: 8 MiB per file,
64 MiB total, and 5,000 files. Ignored datasets, `.git`, `.oms`, symlinks,
nonregular files, and files beyond the bounds are not inherited. Omitted counts
are visible. The snapshot is of the current code state, even when the chosen
conversation is old. A native transcript is not a historical file snapshot.
Unmerged entries, intent-to-add entries, assume-unchanged/skip-worktree flags,
submodules, tracked `.oms` state, and multiply-linked untracked files are refused
because this path cannot faithfully and safely inherit them.

The new checkout receives only a local lineage pointer, not the parent's OMS
room, leases, task records, approvals, or publication authority. Inherited OMS
room, panel, attempt, task, plan, lease, approval and landing environment markers,
plus inherited state-repository and executor routing, are removed for the launch.
Provider credentials, settings, and runtime restrictions remain in the environment.
Native history can contain old instructions; a conversation copy does not grant
new authority to execute them.

The installed executable must advertise the required capabilities in `--help`:

| Provider | Fork action | Resume action |
| --- | --- | --- |
| Claude | `claude --resume ID --fork-session`; `--session-id NEW_ID` when advertised | `claude --resume ID` |
| Codex | `codex fork ID --cd WORKTREE` | `codex resume ID --cd REPO` |
| Pi | `pi --fork EXACT_SESSION_FILE` | `pi --session EXACT_SESSION_FILE` |

Codex's subcommand help must advertise `--cd`; OMS will not guess whether a CLI
build routes a resumed session into the selected checkout. No model, effort,
permission bypass, or automatic prompt is added. The native CLI remains
responsible for its normal authentication, repository trust, and interactive
permission checks. A missing capability stops before creating a checkpoint or
worktree. There is no fallback to editing a provider's transcript.
Managed resumes retain the panel's established launch policy and bootstrap;
the standalone argv table above describes standalone native sessions.

The record distinguishes prepared files, a launch request, process exit, and a
verified new native session ID. Zero exit status alone does not establish a new
session identity. Codex's `forked_from_id` or Pi's `parentSession` must link a new
repository-bound transcript to the selected source. When that evidence is absent
or ambiguous, `native_session` stays `null`. For Claude, an advertised
`--session-id` lets OMS choose an expected new ID, but that ID is still verified
against native metadata before it is reported as the created conversation.

Native conversation and Git state are separate stores. There is no atomic
transaction across them. If the source transcript changes after preparation,
launch is refused and the files remain available; create another fork from the
current conversation. A failed or interrupted launch leaves its checkout,
checkpoint, and recorded launch request available for inspection. OMS does not
retry it automatically because a nonzero exit can still leave native side
effects. Resume the verified child ID explicitly when one is recorded.

Scratch ownership defaults to the invoking process's parent; `--owner-pid PID`
can bind it to another live session process. These exploration worktrees are
Git-locked so ordinary scratch cleanup will preserve them after that process
exits. Keep or move valuable changes before retiring one. To remove a deliberately
retired, clean exploration worktree, unlock it with `git worktree unlock PATH`
and use `oms scratch-worktree remove --repo SOURCE PATH`; that helper refuses
uncommitted changes unless its separate destructive `--force` option is explicit.

## Compare two alternatives

```bash
oms session compare LEFT_FORK_ID RIGHT_FORK_ID --repo .
oms session compare LEFT_FORK_ID RIGHT_FORK_ID --repo . --patch
```

Both records must belong to the selected source repository and share the same
saved HEAD. The command validates each scratch marker, repository root, and
common Git directory. It compares current worktree bytes for paths changed since
the base and bounded untracked files. Separate index and unstaged patch digests
show when two alternatives have the same files but different staging.
Git hook environment variables are cleared for both tracked and untracked
discovery, so an inherited `GIT_DIR` or `GIT_WORK_TREE` cannot redirect the
comparison to the source repository.

Without `--patch`, the report contains relative names, sizes, kinds, executable
bits, and hashes. `--patch` adds bounded unified text differences and withholds
secret-like patches. Binary, symlink, nonregular, and oversized file states are
reported without following links. At most 5,000 changed paths are inspected,
64 MiB of content is read, and 200 differing paths are emitted; patch output is
limited to 16 KiB per path and 64 KiB total.

Files under each fork's `.oms/artifacts` are independently compared as bounded
inventories of relative paths and hashes, up to depth three, 1,000 entries, and
64 MiB. Artifact contents, raw logs, and transcripts are not printed. A hash or
same filename does not mean an artifact passed review or supersedes another
artifact. This command grants no admission or acceptance authority.

## Restore files or conversation independently

```bash
# Existing checkpoint semantics; no native launch.
oms session restore files CHECKPOINT_ID --repo .
oms session restore files CHECKPOINT_ID --repo . --apply
oms session restore files CHECKPOINT_ID --repo . --untracked-only --apply

# Native conversation fork in the current checkout; no OMS file restore.
oms session restore conversation --provider pi --session NATIVE_ID --repo .
oms session restore conversation --provider pi --session NATIVE_ID --repo . --apply
```

`restore files` delegates to `checkpoint.sh`. A tracked restore requires the same
HEAD as checkpoint creation, verifies the saved patches, creates an automatic
recovery checkpoint, and then restores the saved index/worktree state. It never
moves HEAD or rewrites a conversation. Captured untracked files are restored only
where no file already exists. `--untracked-only` preserves tracked state and does
not require the checkpoint HEAD.

`restore conversation` invokes the provider's native fork capability in the
current checkout. OMS does not restore code or rewrite the original conversation.
The new interactive native session can edit files after it starts; this is not a
promise that an interactive model will leave the checkout untouched. To isolate
those later edits, use `session fork` instead.

Local records live under ignored, private `.oms/sessions` paths and are written
atomically through `oms_runtime.common`. Checkpoints retain their existing
private `.oms/checkpoints` location. Records contain local references and hashes,
not conversation excerpts or search queries. They are not portable capsules and
must not be published as such.

## Capability references and verification

The command syntax follows the native providers; installed help is the runtime
gate because versions differ:

- [Claude CLI reference](https://code.claude.com/docs/en/cli-reference)
- [Codex CLI source](https://github.com/openai/codex/blob/main/codex-rs/cli/src/main.rs)
- [Pi coding-agent CLI documentation](https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md)

`python3 -m unittest discover -s tests -p test_sessions.py -v` exercises the public
entrypoint with temporary Git repositories and fake Claude, Codex, and Pi
executables. It checks native argv/cwd/identity, separate file/conversation
restore, staged and unstaged preservation, bounded untracked omission, private
bookmarks, secret filtering, repository scope, source preservation, child and
symlink boundaries, and failure recovery. It makes no real provider calls.
