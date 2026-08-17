# Cross-Agent Consultation

Ask other agents while working, and keep the exchange. A consult is read-only
and cheap; a delegation is a write and is not.

## One command

```bash
oms consult "question"                 # peer that is not you, with context
oms consult --all "question"           # every installed peer, in parallel
oms consult --to codex "question"      # pin the peer
```

It picks a provider other than the caller, attaches the active task, injects
the running conversation, records question and answer, and prints the answer.
Shared memory holds prior conclusions, so it is opt-in (`--memory`): a second
opinion anchored on the first one is not a second opinion. Artifacts land
under `.oms/artifacts/consult`.

For independent answers to the same conceptual or planning question, use the
symmetric council instead of a diff review:

```bash
oms peer-ask --prompt "Compare the two designs for this constraint."
```

Choose the smallest context: none for a concept, `--repo-context` for repository
state, `--diff` for an uncommitted change, or a local summary of specific files.
Use `--debate 1` only when answers materially disagree. Longer debates
(`--debate 2-3`) stay token-bounded on their own: full positions cross once,
later rounds carry only each peer's delta sections plus an on-disk pointer to
the full answer, and the debate stops early when no seat changed position —
so asking for the budgeted rounds is safe when the disagreement warrants
them. If policy forbids direct provider calls, use `--export-only` and import
the answer with `oms artifact-index import`.

For a debate, raise the wall clock before launching — seats doing real
verification die at the default 5m and their round is lost (a dropped seat's
last answer still rides the synthesis, but it cannot rebut anyone):

```bash
OMS_PEER_TIMEOUT=900 oms peer-ask --repo-context --debate 2 \
  --providers codex,claude,antigravity --print-timeout 10m \
  --thread topic-slug --prompt "..."
```

`--providers` also pins models per seat (`codex:model=NAME`); same-provider
seats share one model family, which the reported family count makes explicit.

## Advisor that reads your history

`oms advise` is the decision-point advisor (VERDICT/RISKS/MISSING/NEXT). Add
`--session` to attach a mechanical digest of the current session, so the
advisor judges the decision against what actually happened instead of only
your summary of it — the same read Claude Code's native advisor gets, from
any CLI:

```bash
oms advise --session --prompt "Decision: … Evidence: … Planned next: …"
```

It digests the newest session matching the repo; when two live sessions share
one worktree, pin yours with `--session-id ID`. A sensitive-looking digest is
refused by default because it crosses to another provider; `--allow-sensitive`
overrides that refusal, though agent-call's outbound scrub still applies and
can block the call. Any capture miss fails the call instead of silently
thinning the prompt.

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
