#!/usr/bin/env python3
"""Bounded, read-only search over what OMS already recorded under `.oms`.

`oms journal show --grep TEXT` looks in five kinds of record: Work Journal
events and summaries, handoff digests, thread and room turns, dispatch
transcripts, and panel results. It is a scan, not an index: nothing is
written, no store is built, and provider-native transcripts outside `.oms`
are never opened. Every cap that cut the scan short is reported, so a clean
"no match" is never claimed for records that were not read.

A hit is where to look, not evidence: each printed excerpt first passes the
shared secret scanner, and an excerpt it flags, or any excerpt when the
scanner cannot run, is withheld while its location is still shown.
"""

import argparse
import json
import os
import stat
import subprocess
import sys
import tempfile
import time

KINDS = ("journal", "handoff", "thread", "dispatch", "result")
MAX_FILE_BYTES = 2 * 1024 * 1024   # per file; a longer file is read from its end
MAX_FILES_PER_KIND = 300           # newest first
MAX_COLLECTED = 2000               # matches kept before ranking
TIME_BUDGET = 5.0                  # seconds for the whole scan
DEFAULT_LIMIT, MAX_LIMIT = 20, 200
SNIPPET = 160
QUERY_MAX = 200

SCAN = (
    '. "$1" || exit 2; shift; for f in "$@"; do rc=0; '
    'agent_memory_file_secret_status "$f" 2>/dev/null || rc=$?; printf "%s\\n" "$rc"; done'
)
WITHHELD = "[withheld: sensitive-looking content]"
UNSCANNED = "[withheld: the secret scanner could not run]"


def _regular(path):
    """True for a plain file that is not a symlink."""
    try:
        return stat.S_ISREG(os.lstat(path).st_mode)
    except OSError:
        return False


def _plain_dir(path):
    try:
        return stat.S_ISDIR(os.lstat(path).st_mode)
    except OSError:
        return False


def _files(directory, suffixes, recurse=False):
    """Regular files under `directory` with one of `suffixes`; symlinks are never followed."""
    found = []
    if not _plain_dir(directory):
        return found
    pending = [directory]
    while pending:
        current = pending.pop()
        try:
            names = sorted(os.listdir(current))
        except OSError:
            continue
        for name in names:
            path = os.path.join(current, name)
            if recurse and _plain_dir(path):
                pending.append(path)
            elif name.endswith(suffixes) and _regular(path):
                found.append(path)
    return found


def sources(repo, kinds):
    """(kind, path) for every record file of the wanted kinds, newest first per kind, with the count left out."""
    root = os.path.join(repo, ".oms")
    listing = {
        "journal": lambda: ([p for p in [os.path.join(root, "work-journal", "events.jsonl")] if _regular(p)]
                            + _files(os.path.join(root, "work-journal", "daily"), (".md", ".json"))
                            + _files(os.path.join(root, "work-journal", "weekly"), (".md", ".json"))),
        "handoff": lambda: _files(os.path.join(root, "handoffs"), (".md",)),
        "thread": lambda: _files(os.path.join(root, "threads"), (".jsonl",)),
        "dispatch": lambda: _files(os.path.join(root, "panel-state", "dispatch-logs"), (".log",)),
        "result": lambda: _files(os.path.join(root, "artifacts", "panel-results"), (".md", ".json")),
    }
    chosen, skipped = [], {}
    for kind in KINDS:
        if kind not in kinds:
            continue
        stamped = []
        for path in listing[kind]():
            try:
                stamped.append((os.lstat(path).st_mtime, path))
            except OSError:
                continue
        stamped.sort(reverse=True)
        if len(stamped) > MAX_FILES_PER_KIND:
            skipped[kind] = len(stamped) - MAX_FILES_PER_KIND
        chosen.extend((kind, path, mtime) for mtime, path in stamped[:MAX_FILES_PER_KIND])
    return chosen, skipped


def _read_tail(path):
    """(text, first line number, truncated): at most MAX_FILE_BYTES from the end, starting on a line boundary."""
    try:
        size = os.lstat(path).st_size
        with open(path, "rb") as handle:
            cut = size > MAX_FILE_BYTES
            if cut:
                handle.seek(size - MAX_FILE_BYTES)
            data = handle.read(MAX_FILE_BYTES)
            skipped_lines = 0
            if cut:
                handle.seek(0)
                remaining = size - MAX_FILE_BYTES
                while remaining > 0:
                    block = handle.read(min(1 << 20, remaining))
                    if not block:
                        break
                    skipped_lines += block.count(b"\n")
                    remaining -= len(block)
    except OSError:
        return None, 1, False
    first = 1
    if cut:
        newline = data.find(b"\n")
        data = data[newline + 1:] if newline >= 0 else b""
        first = skipped_lines + 2
    return data.decode("utf-8", "replace"), first, cut


def _strings(value, out, depth=0):
    """String leaves of a decoded JSON row, in order."""
    if depth > 12:
        return
    if isinstance(value, str):
        out.append(value)
    elif isinstance(value, dict):
        for item in value.values():
            _strings(item, out, depth + 1)
    elif isinstance(value, list):
        for item in value:
            _strings(item, out, depth + 1)


def _clean(text):
    return "".join(ch if ch.isprintable() else " " for ch in text)


def _excerpt(text, at, length):
    """One line of at most SNIPPET characters around the match."""
    start = max(0, at - (SNIPPET - length) // 2)
    piece = " ".join(_clean(text[start:start + SNIPPET]).split())
    return ("…" if start > 0 else "") + piece + ("…" if start + SNIPPET < len(text) else "")


def _iso(epoch):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(epoch))


def _row_time(row):
    for key in ("occurred_at", "ts", "time", "created_at", "updated_at"):
        value = row.get(key) if isinstance(row, dict) else None
        if isinstance(value, str) and len(value) >= 10 and value[:4].isdigit():
            return value[:20]
    return None


def _row_id(kind, row, path):
    stem = os.path.splitext(os.path.basename(path))[0]
    if kind == "journal" and isinstance(row, dict) and isinstance(row.get("event_id"), str):
        return _clean(row["event_id"])[:80]
    return _clean(stem)[:80]


def _how(kind, relative, line, ident):
    if kind == "thread":
        return "oms thread show --id %s  (%s:%d)" % (ident, relative, line)
    return "%s:%d" % (relative, line)


def search(repo, query, kinds=KINDS, limit=DEFAULT_LIMIT, clock=time.monotonic):
    """The report dict for `query` (plain text, case-insensitive) over the repo's recorded work."""
    needle = query.casefold()
    started = clock()
    files, skipped_files = sources(repo, kinds)
    hits, caps = [], []
    scanned, truncated, unread, out_of_time, overflow = 0, 0, 0, False, False
    for index, (kind, path, mtime) in enumerate(files):
        if clock() - started > TIME_BUDGET:
            out_of_time = True
            unread = len(files) - index
            break
        text, first, cut = _read_tail(path)
        if text is None:
            continue
        scanned += 1
        truncated += int(cut)
        relative = os.path.relpath(path, repo)
        rows = path.endswith(".jsonl")
        for offset, line in enumerate(text.split("\n")):
            if needle not in line.casefold():
                # A JSON row can hold the text escaped; only rows that might are decoded.
                if not (rows and "\\" in line):
                    continue
            row, haystack = None, line
            if rows:
                try:
                    row = json.loads(line)
                except ValueError:
                    row = None
                if row is not None:
                    leaves = []
                    _strings(row, leaves)
                    haystack = " | ".join(leaves)
            at = haystack.casefold().find(needle)
            if at < 0:
                continue
            if len(hits) >= MAX_COLLECTED:
                overflow = True
                break
            ident = _row_id(kind, row, path)
            hits.append({
                "kind": kind, "when": _row_time(row) or _iso(mtime), "id": ident,
                "path": relative, "line": first + offset,
                "open": _how(kind, relative, first + offset, ident),
                "excerpt": _excerpt(haystack, at, len(needle)),
            })
        if overflow:
            unread = len(files) - index - 1
            break
    hits.sort(key=lambda hit: (hit["when"], hit["path"], hit["line"]), reverse=True)
    shown = hits[:limit]
    _withhold(shown)
    for kind, count in sorted(skipped_files.items()):
        caps.append("%s: %d older file(s) not read (newest %d only)" % (kind, count, MAX_FILES_PER_KIND))
    if truncated:
        caps.append("%d file(s) longer than %d bytes were read from their end only" % (truncated, MAX_FILE_BYTES))
    if out_of_time:
        caps.append("time budget of %gs reached: %d file(s) not read" % (TIME_BUDGET, unread))
    if overflow:
        caps.append("more than %d matches: stopped collecting, %d file(s) not read" % (MAX_COLLECTED, unread))
    return {
        "schema": 1, "kind": "oms-recorded-search", "query": query, "kinds": [k for k in KINDS if k in kinds],
        "files_scanned": scanned, "matches": len(hits), "shown": len(shown), "complete": not caps,
        "caps": caps, "hits": shown,
    }


def _withhold(hits):
    """Replace every excerpt the shared secret scanner does not clear; a scan that fails clears none."""
    if not hits:
        return
    library = os.path.join(os.path.dirname(os.path.abspath(__file__)), "agent-memory-common.sh")
    verdicts = None
    try:
        with tempfile.TemporaryDirectory(prefix="oms-recorded-search.") as directory:
            paths = []
            for index, hit in enumerate(hits):
                path = os.path.join(directory, str(index))
                with open(path, "w", encoding="utf-8") as handle:
                    handle.write(hit["excerpt"] + "\n")
                paths.append(path)
            scan = subprocess.run(["bash", "-c", SCAN, "secret-scan", library, *paths],
                                  capture_output=True, text=True, check=False, timeout=30,
                                  stdin=subprocess.DEVNULL)
        if scan.returncode == 0 and len(scan.stdout.split()) == len(hits):
            verdicts = scan.stdout.split()
    except (OSError, ValueError, subprocess.SubprocessError):
        verdicts = None
    for index, hit in enumerate(hits):
        # 1 is the scanner's "clean"; 0 is a hit and anything else a failed scan.
        if verdicts is None:
            hit["excerpt"], hit["withheld"] = UNSCANNED, True
        elif verdicts[index] != "1":
            hit["excerpt"], hit["withheld"] = WITHHELD, True


def render(report):
    lines = ["recorded-work search for %s: %d match(es) in %d file(s) of %s; showing %d, newest first" % (
        json.dumps(report["query"], ensure_ascii=False), report["matches"], report["files_scanned"],
        ", ".join(report["kinds"]) or "nothing", report["shown"])]
    for hit in report["hits"]:
        lines.append("%-8s %s  %s" % (hit["kind"], hit["when"], hit["id"]))
        lines.append("    %s" % hit["excerpt"])
        lines.append("    open: %s" % hit["open"])
    if report["matches"] > report["shown"]:
        lines.append("%d more match(es) not shown; raise --limit or narrow with --kind" % (
            report["matches"] - report["shown"]))
    for cap in report["caps"]:
        lines.append("incomplete: " + cap)
    lines.append("scope: .oms records only (journal, handoff, thread, dispatch, result); sealed journal segments and "
                 "provider-native transcripts are not read. A hit says where to look, not that anything was verified.")
    return "\n".join(lines)


def run(repo, query, kinds=None, limit=None, as_json=False, stream=None):
    """Exit status: 0 with matches, 1 with none, 2 on a bad request."""
    stream = sys.stdout if stream is None else stream
    query = (query or "").strip()
    if not query or len(query) > QUERY_MAX or "\n" in query:
        print("error: --grep needs 1 to %d characters of plain text on one line" % QUERY_MAX, file=sys.stderr)
        return 2
    wanted = tuple(kinds) if kinds else KINDS
    if any(kind not in KINDS for kind in wanted):
        print("error: --kind is one of: %s" % ", ".join(KINDS), file=sys.stderr)
        return 2
    limit = DEFAULT_LIMIT if limit is None else limit
    if not 1 <= limit <= MAX_LIMIT:
        print("error: --limit is 1 to %d" % MAX_LIMIT, file=sys.stderr)
        return 2
    report = search(os.path.realpath(repo), query, wanted, limit)
    print(json.dumps(report, ensure_ascii=False, sort_keys=True) if as_json else render(report), file=stream)
    return 0 if report["matches"] else 1


def main(argv=None):
    parser = argparse.ArgumentParser(prog="oms journal show --grep", description=__doc__.split("\n\n")[0])
    parser.add_argument("--repo", default=".")
    parser.add_argument("--grep", required=True, metavar="TEXT")
    parser.add_argument("--kind", action="append", choices=KINDS)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    return run(args.repo, args.grep, args.kind, args.limit, args.json)


if __name__ == "__main__":
    sys.exit(main())
