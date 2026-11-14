"""Read-only replay of retained output digests, not provider-token accounting."""

import argparse
import hashlib
import json
import os
from pathlib import Path

from output_capture import CAPTURE_NAME_RE, CAPTURE_ROOT, CaptureError, capture_metadata, read_stored_capture
from output_digest import DEFAULT_MAX_BYTES, build_bytes, render

FILE_CAP = 4 * 1024 * 1024
TOTAL_CAP = 16 * 1024 * 1024
DISCOVERY_CAP = 1000


def capture_names(repo):
    """List only names; content reads independently validate the entire path."""
    path = repo
    for part in CAPTURE_ROOT.parts:
        path = path / part
        if path.is_symlink():
            raise CaptureError("unsafe_capture", "capture store contains a symlink")
        if not path.exists():
            return [], False
        if not path.is_dir():
            raise CaptureError("unsafe_capture", "capture store is not a directory")
    names = []
    with os.scandir(path) as entries:
        for count, entry in enumerate(entries, 1):
            if count > DISCOVERY_CAP:
                # Directory iteration order is unspecified: never claim these
                # are the newest captures after an incomplete scan.
                raise CaptureError("discovery_limit", "capture directory exceeds the 1000-entry discovery limit")
            if CAPTURE_NAME_RE.fullmatch(entry.name):
                names.append(entry.name)
    return sorted(names, reverse=True), True


def report(repo, limit=20):
    repo = Path(repo).resolve()
    names, present = capture_names(repo)
    result = {
        "schema": 1, "kind": "oms-output-replay", "mode": "read-only replay",
        "source": str(CAPTURE_ROOT), "store_present": present,
        "window": {"retained_captures": len(names), "selected_captures": min(limit, len(names)),
                   "limit": limit, "capped": len(names) > limit},
        "configuration": {"max_bytes": DEFAULT_MAX_BYTES, "baseline": "legacy (current loss-marking view)",
                          "candidate": "auto", "per_capture_bytes": FILE_CAP, "total_capture_bytes": TOTAL_CAP,
                          "exit_code": "unknown", "duration_s": "unknown"},
        "totals": {"captures": 0, "source_bytes": 0, "legacy_response_bytes": 0, "auto_response_bytes": 0,
                   "response_bytes_saved": 0, "legacy_complete_captures": 0, "auto_complete_captures": 0},
        "provider_tokens": None, "retrieval_response_bytes": None, "task_success": None,
        "items": [], "skipped": [],
        "limitations": [
            "Replay of retained redacted captures at default settings; original invocation settings, exit and duration are not recorded.",
            "Response bytes include the complete text frame and retrieval reference; a negative saving means auto is larger.",
            "Complete means all source bytes or all JSON values and scalar types are represented; JSON formatting can change.",
            "Provider tokens, cache effects, later retrieval cost and task success are not measured; never verification evidence.",
        ],
    }
    consumed = 0
    for name in names[:limit]:
        remaining = TOTAL_CAP - consumed
        if remaining <= 0:
            result["skipped"].append({"source": str(CAPTURE_ROOT / name), "reason": "total_replay_limit"})
            continue
        try:
            data = read_stored_capture(repo, name, max_bytes=min(FILE_CAP, remaining))
        except CaptureError as error:
            result["skipped"].append({"source": str(CAPTURE_ROOT / name), "reason": error.code})
            continue
        consumed += len(data)
        sha = hashlib.sha256(data).hexdigest()
        item = {"source": str(CAPTURE_ROOT / name), "sha256": sha, "source_bytes": len(data)}
        for strategy in ("legacy", "auto"):
            doc = build_bytes(data, ref=item["source"], strategy=strategy)
            doc["raw"].update(capture_metadata(repo, name, sha))
            item[strategy] = {
                "response_bytes": len(render(doc).encode("utf-8")),
                "complete": doc["representation"]["lossless"],
                "algorithm": doc["representation"]["algorithm"],
                "omitted_lines": doc["shown"]["omitted_lines"],
                "omitted_bytes": doc["shown"]["omitted_bytes"],
            }
        item["response_bytes_saved"] = item["legacy"]["response_bytes"] - item["auto"]["response_bytes"]
        result["items"].append(item)
        totals = result["totals"]
        totals["captures"] += 1
        totals["source_bytes"] += len(data)
        totals["response_bytes_saved"] += item["response_bytes_saved"]
        for strategy in ("legacy", "auto"):
            totals[strategy + "_response_bytes"] += item[strategy]["response_bytes"]
            totals[strategy + "_complete_captures"] += int(item[strategy]["complete"])
    return result


def emit(result):
    total = result["totals"]
    window = result["window"]
    print("output replay: %d/%d retained capture(s); default text budget %d bytes" % (
        total["captures"], window["retained_captures"], DEFAULT_MAX_BYTES))
    if not result["store_present"]:
        print("not recorded: no output-digest capture directory")
    print("complete response bytes: legacy=%d auto=%d saved=%d (negative means larger)" % (
        total["legacy_response_bytes"], total["auto_response_bytes"], total["response_bytes_saved"]))
    print("complete captures: legacy=%d auto=%d" % (
        total["legacy_complete_captures"], total["auto_complete_captures"]))
    for item in result["items"]:
        print("  saved=%d legacy=%d auto=%d algorithm=%s complete=%s source=%s" % (
            item["response_bytes_saved"], item["legacy"]["response_bytes"], item["auto"]["response_bytes"],
            item["auto"]["algorithm"], str(item["auto"]["complete"]).lower(), item["source"]))
    for item in result["skipped"]:
        print("  skipped=%s source=%s" % (item["reason"], item["source"]))
    if window["capped"]:
        print("window capped: only the newest %d generated filenames were selected" % window["limit"])
    for note in result["limitations"]:
        print(note)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()
    if not 1 <= args.limit <= 50:
        parser.error("--limit must be 1..50")
    try:
        result = report(Path(args.repo), args.limit)
    except (CaptureError, OSError) as error:
        parser.exit(2, "output replay: %s\n" % (error.code if isinstance(error, CaptureError) else "capture_unreadable"))
    if args.json:
        print(json.dumps(result, ensure_ascii=False, sort_keys=True))
    else:
        emit(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
