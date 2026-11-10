"""Loss-aware metadata digest of an explicitly selected local tool-output log.

The raw file is never rewritten. Pattern matches are locators, not assertions
that a process failed. Content is not emitted, sent to a model or executed.
"""
from __future__ import annotations

from collections import Counter
import hashlib
import os
from pathlib import Path
import re
import stat

READ_DEFAULT = 1024 * 1024
READ_MAX = 8 * 1024 * 1024
LINE_MAX = 64 * 1024
LINE_COUNT_MAX = 100_000
ANSI = re.compile(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07\x1b]*(?:\x07|\x1b\\))")
PATTERNS = {
    "traceback": re.compile(r"\bTraceback \(most recent call last\):"),
    "error": re.compile(r"(?:^|\s)(?:error|fatal)(?:\s*[:\[]|\b)", re.I),
    "test_failure": re.compile(r"(?:^FAILED\s|^FAIL(?:ED)?[:\s]|\b[1-9][0-9]* failed\b)", re.I),
    "memory": re.compile(r"\b(?:out of memory|OutOfMemoryError|OOM)\b", re.I),
    "nonfinite": re.compile(r"\b(?:NaN|non[- ]finite|nan loss)\b", re.I),
    "process_signal": re.compile(r"\b(?:Segmentation fault|SIG(?:KILL|SEGV|ABRT)|Killed)\b"),
}


class DigestError(ValueError):
    """Unsafe, malformed or changing log; errors omit source path and contents."""


def _arguments(read_bytes, limit, exit_code, window):
    if type(read_bytes) is not int or not 512 <= read_bytes <= READ_MAX:
        raise DigestError("read-bytes must be from 512 to 8388608")
    if type(limit) is not int or not 1 <= limit <= 100:
        raise DigestError("limit must be from 1 to 100")
    if exit_code is not None and (type(exit_code) is not int or not -255 <= exit_code <= 255):
        raise DigestError("reported-exit must be an integer from -255 to 255")
    if window not in ("head", "tail"):
        raise DigestError("window must be head or tail")


def read_window(repo, filename, *, read_bytes=READ_DEFAULT, window="tail"):
    """Read at most read_bytes, anchored below repo, refusing every link component."""
    _arguments(read_bytes, 1, None, window)
    if not hasattr(os, "O_NOFOLLOW") or os.open not in os.supports_dir_fd or not hasattr(os, "getuid"):
        raise DigestError("safe descriptor-relative log reads are unavailable")
    fds = []
    try:
        root = Path(repo).resolve(strict=True)
        path = Path(filename)
        if path.is_absolute():
            path = path.relative_to(root)
        if not path.parts or ".." in path.parts or len(path.parts) > 64:
            raise DigestError("log must be an explicit repo-local path without parent traversal")
        directory_flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
        fds.append(os.open(root, directory_flags))
        for component in path.parts[:-1]:
            fds.append(os.open(component, directory_flags, dir_fd=fds[-1]))
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fds[-1])
        fds.append(fd)
        before = os.fstat(fd)
        if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_uid != os.getuid():
            raise DigestError("log must be an owned regular file without links")
        start = max(0, before.st_size - read_bytes) if window == "tail" else 0
        size = min(read_bytes, before.st_size - start)
        os.lseek(fd, start, os.SEEK_SET)
        remaining = size
        pieces = []
        while remaining:
            piece = os.read(fd, min(remaining, 65536))
            if not piece:
                raise DigestError("log became shorter while being read")
            pieces.append(piece)
            remaining -= len(piece)
        after = os.fstat(fd)
        fields = ("st_dev", "st_ino", "st_size", "st_mtime_ns", "st_ctime_ns", "st_uid", "st_nlink")
        if any(getattr(before, key) != getattr(after, key) for key in fields):
            raise DigestError("log changed while being read; retry the explicit read later")
        return b"".join(pieces), start, before.st_size
    except (OSError, TypeError, ValueError) as exc:
        if isinstance(exc, DigestError):
            raise
        raise DigestError("cannot safely read the selected repo-local log") from None
    finally:
        for fd in reversed(fds):
            os.close(fd)


def summarize(raw, start, total_bytes, *, limit=20, exit_code=None):
    """Summarize complete bounded lines only; SHA identifies raw observed bytes."""
    _arguments(READ_DEFAULT, limit, exit_code, "tail")
    if (not isinstance(raw, bytes) or len(raw) > READ_MAX or type(start) is not int
            or type(total_bytes) is not int or start < 0 or total_bytes < start + len(raw)):
        raise DigestError("invalid log window")
    digest = hashlib.sha256(raw).hexdigest()
    full = start == 0 and len(raw) == total_bytes
    leading = 0
    if start:
        # The preceding byte is not read. Conservatively omit the first segment
        # even when start happened to fall at a line boundary.
        newline = raw.find(b"\n")
        leading = len(raw) if newline < 0 else newline + 1
    last_newline = raw.rfind(b"\n") + 1
    end = max(leading, last_newline)
    complete = raw[leading:end]
    trailing = len(raw) - end
    counters = Counter()
    hits = []
    hit_count = skipped = scanned_lines = 0
    complete_count = complete.count(b"\n")
    cursor = 0
    # Walk LF records without allocating a list proportional to all log lines.
    # Only the requested candidate rows are retained; counts cover scanned rows.
    while cursor < len(complete) and scanned_lines < LINE_COUNT_MAX:
        line_end = complete.find(b"\n", cursor)
        line = complete[cursor:line_end]
        offset = start + leading + cursor
        length = line_end + 1 - cursor
        scanned_lines += 1
        if len(line) > LINE_MAX:
            skipped += 1
        else:
            try:
                text = line.decode("utf-8")
            except UnicodeDecodeError:
                raise DigestError("complete log lines are not UTF-8") from None
            if "\x00" in text:
                raise DigestError("selected log window contains binary data")
            text = ANSI.sub("", text)
            labels = [name for name, pattern in PATTERNS.items() if pattern.search(text)]
            if labels:
                counters.update(labels)
                hit_count += 1
                if len(hits) < limit:
                    hits.append({"byte_start": offset, "byte_end": offset + length,
                                 "line_in_window": scanned_lines,
                                 "file_line": scanned_lines if start == 0 else None,
                                 "classes": labels, "line_sha256": hashlib.sha256(line).hexdigest()})
        cursor = line_end + 1
    unscanned_lines = complete_count - scanned_lines
    return {"schema": 1, "kind": "oms-output-digest", "read_only": True,
            "raw_rewritten": False, "content_included": False,
            "window": {"byte_start": start, "byte_end": start + len(raw),
                       "observed_file_bytes": total_bytes, "read_bytes": len(raw),
                       "not_read_bytes": total_bytes - len(raw), "sha256": digest,
                       "full_file_sha256": digest if full else None,
                       "leading_partial_bytes": leading, "trailing_unterminated_bytes": trailing,
                       "complete_lines": complete_count, "scanned_lines": scanned_lines,
                       "unscanned_lines": unscanned_lines, "oversized_lines_skipped": skipped,
                       "scan_complete": full and not leading and not trailing and not skipped and not unscanned_lines},
            "reported_exit": {"code": exit_code,
                              "source": "caller-supplied-unverified" if exit_code is not None else "unreported"},
            "candidates": {"total": hit_count, "items": hits, "omitted": hit_count - len(hits)},
            "class_counts": dict(sorted(counters.items())),
            "diagnosis_complete": False, "process_success": None,
            "note": "Pattern hits are locators, not a verdict. No hits does not prove success. Raw output and verifier inputs are unchanged; no token, billing or quota savings are inferred."}


def inspect_file(repo, filename, *, read_bytes=READ_DEFAULT, window="tail", limit=20, exit_code=None):
    _arguments(read_bytes, limit, exit_code, window)
    raw, start, total = read_window(repo, filename, read_bytes=read_bytes, window=window)
    return summarize(raw, start, total, limit=limit, exit_code=exit_code)


def render(report):
    info = report["window"]
    lines = ["OMS output digest (metadata only)", report["note"],
             "raw observed bytes: %d..%d / %d; %d not read" %
             (info["byte_start"], info["byte_end"], info["observed_file_bytes"], info["not_read_bytes"]),
             "window sha256: " + info["sha256"],
             "whole-file sha256: " + (info["full_file_sha256"] or "not computed"),
             "complete lines: %d; oversized skipped: %d; partial bytes: %d leading / %d trailing" %
             (info["complete_lines"], info["oversized_lines_skipped"], info["leading_partial_bytes"], info["trailing_unterminated_bytes"]),
             "line cap: %d scanned / %d unscanned" % (info["scanned_lines"], info["unscanned_lines"]),
             "scan complete: %s; diagnosis complete: no" % ("yes" if info["scan_complete"] else "no"),
             "reported exit: %s (%s)" % (report["reported_exit"]["code"] if report["reported_exit"]["code"] is not None else "unknown",
                                          report["reported_exit"]["source"])]
    for row in report["candidates"]["items"]:
        lines.append("candidate bytes %d..%d: %s" % (row["byte_start"], row["byte_end"], ", ".join(row["classes"])))
    lines.append("candidates: %d total / %d shown / %d omitted" %
                 (report["candidates"]["total"], len(report["candidates"]["items"]), report["candidates"]["omitted"]))
    return "\n".join(lines) + "\n"


def register(commands, shared_options):
    parser = commands.add_parser("output-digest", help="inspect one saved log without changing raw output", parents=[shared_options])
    parser.add_argument("log", help="explicit log inside --repo")
    parser.add_argument("--read-bytes", type=int, default=READ_DEFAULT, help="read cap, 512..8388608")
    parser.add_argument("--window", choices=("head", "tail"), default="tail")
    parser.add_argument("--limit", type=int, default=20, help="candidate locators, 1..100")
    parser.add_argument("--reported-exit", type=int, help="caller-supplied code; not independently verified")
    parser.add_argument("--format", choices=("json", "text"), default="json")


def dispatch(args, repo, emit):
    from .common import CoreError
    if args.format == "text" and args.pretty:
        raise CoreError("--pretty applies to JSON, not --format text")
    try:
        report = inspect_file(repo, args.log, read_bytes=args.read_bytes, window=args.window,
                              limit=args.limit, exit_code=args.reported_exit)
    except DigestError as exc:
        raise CoreError(str(exc)) from None
    if args.format == "text":
        print(render(report), end="")
    else:
        emit(report, args.pretty)
    return 0
