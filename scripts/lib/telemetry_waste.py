"""Read-only "where did tokens go to waste" view for `artifact-index telemetry --waste`.

Everything here is derived from rows that already exist: the artifact index
rows the caller passes in, the hook event ledger and the output-digest
capture directory. Nothing is written, priced or estimated. A category with
no recorded source says so (`not-recorded`); that is never a claim that
nothing was wasted. Values are as reported, never approval evidence."""

from __future__ import annotations

import json
import os
import stat
from collections import defaultdict
from typing import Callable, Optional

LEDGER = ".oms/hooks/events.jsonl"
DIGEST_DIR = ".oms/artifacts/output-digest"
MAX_TOP = 100
CATEGORIES = ("unadmitted_delegate_tokens", "repeated_failed_delegations", "model_switches",
              "hook_injected_bytes", "stored_output_bytes")
BASIS = ("reported values from retained rows, never approval or verification evidence; "
         "a category that is absent or `not-recorded` means the data was not recorded, "
         "not that nothing was wasted")


def _count(value: object) -> Optional[int]:
    return value if type(value) is int and value >= 0 else None


def _text(value: object) -> Optional[str]:
    return value if isinstance(value, str) and value else None


def _share(part: int, whole: int) -> Optional[float]:
    return round(part / whole, 4) if whole else None


def _category(status: str, unit: str, **fields: object) -> dict:
    return dict({"status": status, "unit": unit}, **fields)


def _source(row: dict, position: int) -> str:
    return _text(row.get("event_id")) or "index-row-%d" % position


def failed(row: dict) -> bool:
    """A recorded non-zero exit or non-zero verification; absence of both is not failure."""
    if (_count(row.get("exit")) or 0) > 0:
        return True
    if row.get("context_verification") in ("none", "dry-run"):
        return False
    return (_count(row.get("verify_exit")) or 0) > 0


def _delegates(rows: list, positions: dict) -> list:
    return [(positions[id(row)], row) for row in rows if row.get("kind") == "delegate"]


def unadmitted(delegates: list, admitted: set, tokens_of: Callable, model_of: Callable, top: int) -> dict:
    reported = total = 0
    items = []
    for position, row in delegates:
        tokens = tokens_of(row)
        if tokens is None:
            continue
        total += tokens
        digest = row.get("patch_sha256")
        if isinstance(digest, str) and digest in admitted:
            continue
        reported += tokens
        items.append({"tokens": tokens, "role": _text(row.get("role")) or "unknown",
                      "provider": _text(row.get("provider")) or "unrecorded", "model": model_of(row),
                      "source": _source(row, position), "artifact": _text(row.get("artifact"))})
    if not total:
        return _category("not-recorded", "tokens", reason="no delegate row reports tokens")
    items.sort(key=lambda item: (-item["tokens"], item["source"]))
    return _category("recorded", "tokens", tokens=reported, of_total=total, share=_share(reported, total),
                     rows=len(items), items=items[:top],
                     note="delegate rows whose patch no later patch-admit/patch-land accepted; "
                          "includes delegations still awaiting admission")


def repeated_failures(delegates: list, admitted: set, tokens_of: Callable, top: int) -> dict:
    """Delegations sharing a brief digest, or a task id, where every attempt failed."""
    groups: dict = defaultdict(list)
    total = 0
    for position, row in delegates:
        tokens = tokens_of(row)
        total += tokens or 0
        digest = row.get("patch_sha256")
        row_failed = failed(row) and not (isinstance(digest, str) and digest in admitted)
        for field, kind in (("prompt_sha256", "brief"), ("task_id", "task")):
            key = _text(row.get(field))
            if key:
                groups[(kind, key)].append((position, row, tokens, row_failed))
    if not any(_text(row.get("prompt_sha256")) or _text(row.get("task_id")) for _, row in delegates):
        return _category("not-recorded", "tokens", reason="no delegate row carries a brief digest or task id")
    items, counted = [], {}
    for (kind, key), members in groups.items():
        if len(members) < 2 or not all(member[3] for member in members):
            continue
        tokens = sum(member[2] or 0 for member in members)
        for position, _row, own, _failed in members:
            counted[position] = own or 0
        items.append({"by": kind, "key": key[:16], "attempts": len(members), "tokens": tokens,
                      "token_reports": sum(member[2] is not None for member in members),
                      "providers": sorted({_text(member[1].get("provider")) or "unrecorded" for member in members}),
                      "sources": [_source(member[1], member[0]) for member in members][:5]})
    items.sort(key=lambda item: (-item["tokens"], -item["attempts"], item["by"], item["key"]))
    reported = sum(counted.values())
    return _category("recorded", "tokens", tokens=reported, of_total=total, share=_share(reported, total),
                     groups=len(items), items=items[:top],
                     note="every attempt in the group failed (non-zero exit or verification, patch not admitted); "
                          "a brief and a task group can list the same rows, the total counts each row once")


def _ledger_rows(repo: str):
    path = os.path.join(repo, *LEDGER.split("/"))
    if os.path.islink(path) or not os.path.isfile(path):
        return None
    return path


def model_switches(repo: str, top: int) -> dict:
    """Consecutive main-session hook telemetry rows whose recorded model differs.

    The ledger carries no reasoning effort, and an index row carries no session,
    so only a model change can be seen. The cache rebuild is the reason it is
    listed, not something the ledger measured."""
    path = _ledger_rows(repo)
    if path is None:
        return _category("not-recorded", "events", reason="no hook event ledger")
    last: dict = {}
    pairs = 0
    items = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except (TypeError, ValueError):
                continue
            session, model = (row.get("session"), row.get("model")) if isinstance(row, dict) else (None, None)
            if (row.get("action") != "telemetry" or not _text(session) or not _text(model)
                    or row.get("agent_id_hash") or row.get("subagent_type")):
                continue
            before = last.get(session)
            last[session] = model
            if before is None:
                continue
            pairs += 1
            if before != model:
                items.append({"session": session, "from": before, "to": model, "ts": _text(row.get("ts")),
                              "cache_creation_tokens": _count(row.get("cache_creation_tokens")),
                              "source": "%s session=%s ts=%s" % (LEDGER, session, _text(row.get("ts")) or "undated")})
    if not pairs:
        return _category("not-recorded", "events", reason="no session has two hook telemetry rows that record a model")
    items.sort(key=lambda item: (-(item["cache_creation_tokens"] or 0), item["ts"] or "", item["session"]))
    return _category("recorded", "events", switches=len(items), of_transitions=pairs,
                     share=_share(len(items), pairs), items=items[:top],
                     note="share is of consecutive same-session model-recording rows, not tokens; "
                          "cache_creation_tokens is shown only where the row reported it; effort changes are not recorded")


def hook_injections(repo: str, top: int) -> dict:
    """Largest injected byte totals per session, from the existing injection ledger reader."""
    if _ledger_rows(repo) is None:
        return _category("not-recorded", "bytes", reason="no hook event ledger")
    try:
        from oms_runtime.context_session import inspect_injections
        block = inspect_injections(repo, limit=top)
    except Exception as error:  # an oversized or unreadable ledger is reported, not guessed around
        return _category("not-recorded", "bytes", reason="ledger unreadable: %s" % error)
    if not block["injections"]:
        return _category("not-recorded", "bytes", reason="the ledger holds no injection rows")
    items = [{"session": entry["session"], "bytes": entry["bytes"], "calls": entry["calls"],
              "largest": entry["largest"], "source": "%s session=%s" % (LEDGER, entry["session"])}
             for entry in block["by_session"]["items"]]
    listed = sum(item["bytes"] for item in items)
    return _category("recorded", "bytes", bytes=listed, of_total=block["bytes"], share=_share(listed, block["bytes"]),
                     sessions=block["by_session"]["total"], items=items,
                     note="the top sessions' share of every byte OMS hooks recorded injecting; "
                          "utf-8 bytes, not tokens; hooks outside the ledger are not counted")


def stored_outputs(repo: str, top: int) -> dict:
    """Largest captures kept by output-digest; sizes only, contents are never read."""
    directory = os.path.join(repo, *DIGEST_DIR.split("/"))
    if os.path.islink(directory) or not os.path.isdir(directory):
        return _category("not-recorded", "bytes", reason="no output-digest capture directory")
    items, total = [], 0
    with os.scandir(directory) as entries:
        for entry in entries:
            try:
                info = entry.stat(follow_symlinks=False)
            except OSError:
                continue
            if not stat.S_ISREG(info.st_mode):
                continue
            total += info.st_size
            items.append({"bytes": info.st_size, "source": "%s/%s" % (DIGEST_DIR, entry.name)})
    if not items:
        return _category("not-recorded", "bytes", reason="the capture directory is empty")
    items.sort(key=lambda item: (-item["bytes"], item["source"]))
    shown = items[:top]
    listed = sum(item["bytes"] for item in shown)
    return _category("recorded", "bytes", bytes=listed, of_total=total, share=_share(listed, total),
                     files=len(items), items=shown,
                     note="largest raw captures kept on disk; bytes, not tokens, and the newest 50 are all it keeps")


def waste_report(repo: str, selected: list, all_rows: list, admitted: set,
                 tokens_of: Callable, model_of: Callable, top: int) -> dict:
    positions = {id(row): number for number, row in enumerate(all_rows, 1)}
    delegates = _delegates(selected, positions)
    return {
        "schema": 1, "top": top, "basis": BASIS,
        "windowing": "categories 1-2 use the selected index rows; 3-5 read the hook ledger and capture "
                     "directory as they are, ignoring --since/--until",
        "categories": {
            "unadmitted_delegate_tokens": unadmitted(delegates, admitted, tokens_of, model_of, top),
            "repeated_failed_delegations": repeated_failures(delegates, admitted, tokens_of, top),
            "model_switches": model_switches(repo, top),
            "hook_injected_bytes": hook_injections(repo, top),
            "stored_output_bytes": stored_outputs(repo, top),
        },
    }


def _percent(share: Optional[float]) -> str:
    return "-" if share is None else "%.1f%%" % (share * 100)


def emit(report: dict) -> None:
    print("token waste view (top %d per category)" % report["top"])
    print(report["basis"])
    print(report["windowing"])
    cats = report["categories"]

    def head(label: str, block: dict, detail: str) -> bool:
        if block["status"] != "recorded":
            print("%s: not recorded (%s)" % (label, block["reason"]))
            return False
        print("%s: %s -- share %s; %s" % (label, detail, _percent(block["share"]), block["note"]))
        return True

    block = cats["unadmitted_delegate_tokens"]
    if head("1 unadmitted delegate tokens", block,
            "%d of %d reported delegate tokens over %d row(s)" % (block.get("tokens", 0), block.get("of_total", 0), block.get("rows", 0))):
        for item in block["items"]:
            print("  %d tokens  role=%s provider=%s model=%s  source=%s%s" % (
                item["tokens"], item["role"], item["provider"], item["model"], item["source"],
                " artifact=" + item["artifact"] if item["artifact"] else ""))
    block = cats["repeated_failed_delegations"]
    if head("2 repeated failed delegations", block,
            "%d of %d reported delegate tokens in %d group(s)" % (block.get("tokens", 0), block.get("of_total", 0), block.get("groups", 0))):
        for item in block["items"]:
            print("  %d tokens  %d attempts  %s=%s  providers=%s  sources=%s" % (
                item["tokens"], item["attempts"], item["by"], item["key"], ",".join(item["providers"]), ",".join(item["sources"])))
    block = cats["model_switches"]
    if head("3 model switches within a session", block,
            "%d of %d transitions" % (block.get("switches", 0), block.get("of_transitions", 0))):
        for item in block["items"]:
            print("  %s -> %s  cache-creation=%s  source=%s" % (
                item["from"], item["to"], "-" if item["cache_creation_tokens"] is None else item["cache_creation_tokens"], item["source"]))
    block = cats["hook_injected_bytes"]
    if head("4 hook-injected bytes by session", block,
            "%d of %d bytes in the top session(s), %d session(s) in all" % (block.get("bytes", 0), block.get("of_total", 0), block.get("sessions", 0))):
        for item in block["items"]:
            print("  %d bytes  %d call(s)  largest=%d  source=%s" % (item["bytes"], item["calls"], item["largest"], item["source"]))
    block = cats["stored_output_bytes"]
    if head("5 largest stored outputs", block,
            "%d of %d bytes in the largest file(s), %d file(s) in all" % (block.get("bytes", 0), block.get("of_total", 0), block.get("files", 0))):
        for item in block["items"]:
            print("  %d bytes  source=%s" % (item["bytes"], item["source"]))
