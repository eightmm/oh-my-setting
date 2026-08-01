# Cross-Agent Consultation

Ask other agents while working, and keep the exchange. A consult is read-only
and cheap; a delegation is a write and is not.

## One command

```bash
oms consult "question"                 # peer that is not you, with context
oms consult --all "question"           # every installed peer, in parallel
oms consult --to codex "question"      # pin the peer
```

It picks a provider other than the caller, attaches the active task and shared
memory, injects the running conversation, records question and answer, and
prints the answer. Artifacts land under `.oms/artifacts/consult`.

For independent answers to the same conceptual or planning question, use the
symmetric council instead of a diff review:

```bash
oms peer-ask --prompt "Compare the two designs for this constraint."
```

Choose the smallest context: none for a concept, `--repo-context` for repository
state, `--diff` for an uncommitted change, or a local summary of specific files.
Use `--debate 1` only when answers materially disagree. If policy forbids direct
provider calls, use `--export-only` and import the answer with
`oms artifact-index import`.

## Consult during work, not only at gates

Consult when another model's independent view changes what you do next:

- A design choice with more than one defensible answer, before you build on it.
- A failure you have already retried once — a second model reads the same
  evidence differently.
- An unfamiliar subsystem: ask what breaks before editing it.
- Conflicting evidence between docs, code, and test behavior.

Do not consult to confirm work that is already verified, to relay a question the
repo answers, or in a loop — one consult, then act on it.

## Conversations, not one-shot questions

Every consult joins a thread, so the next question starts from what was already
said and any provider can pick it up in a later session.

```bash
oms thread list                     # open conversations, newest first
oms thread show --id ID             # full transcript
oms thread new --topic "..."        # start a separate conversation
oms thread close --id ID --summary "decided X"
```

`--thread ID` also works on `oms agent-call`, `oms agent-run`,
`oms peer-ask`, `oms peer-delegate`, and `oms advise`, so a council answer, a
delegated patch outcome, and an advisor verdict all land in the same
conversation instead of in three unrelated artifacts. Naming a thread that does
not exist creates it.

Record your own decisions in the thread when they close a question a peer
answered — the next agent then sees the conclusion, not only the debate:

```bash
oms thread append --role decision --text "going with map-style; codex's DDP
concern handled by per-worker sharding"
```

## Boundaries

- Consults are read-only. For a write, use `oms peer-delegate` (isolated
  worktree, reviewable patch) or `oms agent-run --mode write`.
- Turns are replayed into other providers' prompts, so the same sensitive-content
  gate as shared memory applies: no secrets, private paths, or cluster details.
  A refused turn does not fail the call; the artifact still holds the full text.
- Threads are repo-local state under `.oms/threads/`, never committed. `oms gc`
  sweeps closed threads; open ones are kept.
- The caller still owns the decision. A peer answer is evidence, not authority,
  and never widens scope, authority, or verification.
