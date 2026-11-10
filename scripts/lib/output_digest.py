"""A loss-marking digest of one captured command output.

The raw capture is never rewritten: this reads it and prints a bounded view
that says what it read, what it left out, the exit code and the failure lines
it recognised. No model is called, so the same bytes always give the same
digest. A log no recogniser understands falls back to a bounded head and tail.
"""

import argparse
import hashlib
import json
import os
import re
import sys

SCHEMA = 1
LINE_CHARS = 300
FAILURE_LINES = 40
DEFAULT_MAX_BYTES = 4000
DEFAULT_HEAD = 10
DEFAULT_TAIL = 30
READ_CAP = 64 * 1024 * 1024

ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]")
CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f\x7f]")

# (recogniser name, pattern). A line counts once, for the first that matches.
RECOGNISERS = (
    ("pytest", re.compile(r"^(?:FAILED|ERROR) \S+(?:::\S+)?(?: - |$)|^=+ .*\b\d+ (?:failed|error)")),
    ("unittest", re.compile(r"^(?:FAIL|ERROR): \S+ \(|^FAILED \((?:failures|errors)=")),
    ("smoke", re.compile(r"^FAIL: |^check: \S+ FAILED|^not ok \d+")),
    ("traceback", re.compile(r"^Traceback \(most recent call last\):|^\w+(?:\.\w+)*(?:Error|Exception): ")),
    ("compiler", re.compile(r"^\S+:\d+(?::\d+)?: (?:fatal )?error\b|^error(?:\[\w+\])?: |^make(?:\[\d+\])?: \*\*\* ")),
    ("shellcheck", re.compile(r"\bSC\d{4} \((?:error|warning)\)")),
    ("lint", re.compile(r"^\S+:\d+:\d+: [EFW]\d+ |^\s*\d+:\d+\s+error\s")),
    ("runtime", re.compile(r"\b(?:Segmentation fault|Killed|Out of memory|OOM|command not found|"
                           r"No such file or directory|Permission denied|panic:|FATAL)\b")),
    ("generic", re.compile(r"\b(?:ERROR|Error:|error:|FAILED|FAILURE|Failed:)")),
)


def clean(line):
    """One raw line as it is shown: escape sequences and control bytes removed, bounded in length."""
    line = CONTROL.sub("", ANSI.sub("", line)).rstrip()
    if len(line) > LINE_CHARS:
        line = "%s…[+%d chars]" % (line[:LINE_CHARS], len(line) - LINE_CHARS)
    return line


def read(path):
    size = os.path.getsize(path)
    sha = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            sha.update(block)
    skipped = 0
    with open(path, "rb") as handle:
        if size > READ_CAP:
            # A capture past the cap is read from its end: the ending is where a run says how it went.
            skipped = size - READ_CAP
            handle.seek(skipped)
            handle.readline()
            skipped = handle.tell()
        data = handle.read()
    text = data.decode("utf-8", "replace")
    partial = bool(text) and not text.endswith("\n")
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return {"bytes": size, "sha256": sha.hexdigest(), "lines": lines, "partial_last_line": partial,
            "unread_leading_bytes": skipped}


def failures(lines):
    found, by = [], {}
    for number, raw in enumerate(lines, 1):
        line = ANSI.sub("", raw)
        for name, pattern in RECOGNISERS:
            if pattern.search(line):
                found.append((number, name))
                by[name] = by.get(name, 0) + 1
                break
    return found, by


def build(path, exit_code=None, max_bytes=DEFAULT_MAX_BYTES, head=DEFAULT_HEAD, tail=DEFAULT_TAIL,
          ref=None, duration=None, withhold=None):
    raw = read(path)
    lines = raw["lines"]
    total = len(lines)
    found, by = failures(lines)
    doc = {
        "schema": SCHEMA, "kind": "oms-output-digest",
        "exit_code": exit_code, "duration_s": duration,
        "raw": {"ref": ref, "sha256": raw["sha256"], "bytes": raw["bytes"], "lines": total,
                "partial_last_line": raw["partial_last_line"],
                "unread_leading_bytes": raw["unread_leading_bytes"]},
        "parser": sorted(by) if by else ["none"],
        "failures": {"matched": len(found), "by_recogniser": by},
        "withheld": withhold,
    }
    whole = sum(len(line.encode("utf-8")) + 1 for line in (clean(l) for l in lines))
    out = []
    if withhold:
        doc["shown"] = {"mode": "withheld", "lines": 0, "omitted_lines": total, "omitted_bytes": raw["bytes"]}
        doc["failures"]["shown"] = 0
        doc["text"] = ""
        return doc
    if whole <= max_bytes and not raw["unread_leading_bytes"]:
        out = [clean(line) for line in lines]
        doc["shown"] = {"mode": "whole", "lines": total, "omitted_lines": 0, "omitted_bytes": 0}
        doc["failures"]["shown"] = len(found)
        doc["text"] = "\n".join(out)
        return doc
    # Budget order: the failure lines first, then the tail, then the head; each is cut, never the exit code.
    budget = max_bytes
    shown = {}

    def take(number):
        nonlocal budget
        if number in shown or not 1 <= number <= total:
            return number in shown
        line = clean(lines[number - 1])
        cost = len(line.encode("utf-8")) + len(str(number)) + 3
        if cost > budget:
            return False
        budget -= cost
        shown[number] = line
        return True

    kept_failures = 0
    for number, _name in found[:FAILURE_LINES]:
        if not take(number):
            break
        kept_failures += 1
        take(number + 1)  # the line under a failure header usually carries the reason
    tail_start = max(1, total - tail + 1)
    for number in range(total, tail_start - 1, -1):
        if not take(number):
            break
    for number in range(1, min(head, total) + 1):
        if not take(number):
            break
    previous = 0
    for number in sorted(shown):
        if number != previous + 1:
            gap = number - previous - 1
            out.append("   … %d line(s) omitted …" % gap)
        out.append("%d| %s" % (number, shown[number]))
        previous = number
    if previous < total:
        out.append("   … %d line(s) omitted …" % (total - previous))
    text = "\n".join(out)
    doc["shown"] = {"mode": "failures+tail+head" if by else "raw head+tail",
                    "lines": len(shown), "omitted_lines": total - len(shown),
                    "omitted_bytes": max(0, raw["bytes"] - sum(len(lines[n - 1].encode("utf-8")) + 1 for n in shown))}
    doc["failures"]["shown"] = kept_failures
    doc["text"] = text
    return doc


def render(doc):
    raw, shown, fails = doc["raw"], doc["shown"], doc["failures"]
    code = doc["exit_code"]
    head = "output-digest: exit=%s  lines=%d  bytes=%d" % ("unknown" if code is None else code, raw["lines"], raw["bytes"])
    if doc.get("duration_s") is not None:
        head += "  duration=%ss" % doc["duration_s"]
    out = [head]
    out.append("raw: %s  sha256=%s" % (raw["ref"] or "not kept", raw["sha256"]))
    if raw["unread_leading_bytes"]:
        out.append("read: the last %d bytes; the first %d bytes were not read"
                   % (raw["bytes"] - raw["unread_leading_bytes"], raw["unread_leading_bytes"]))
    if raw["partial_last_line"]:
        out.append("note: the capture ends mid-line (no final newline)")
    if doc["withheld"]:
        out.append("shown: nothing; %s" % doc["withheld"])
        out.append("failures: %d matching line(s)%s" % (
            fails["matched"], " (%s)" % _by(fails) if fails["matched"] else ""))
    else:
        if shown["mode"] == "whole":
            out.append("shown: all %d line(s); nothing omitted" % shown["lines"])
        else:
            out.append("shown: %d of %d line(s) (%s); omitted %d line(s), %d bytes — open the raw file for them"
                       % (shown["lines"], raw["lines"], shown["mode"], shown["omitted_lines"], shown["omitted_bytes"]))
        if fails["matched"]:
            out.append("failures: %d matching line(s) (%s); %d shown" % (fails["matched"], _by(fails), fails["shown"]))
        elif code not in (None, 0):
            out.append("failures: no recogniser matched; the lines below are a raw head and tail, not a diagnosis")
        else:
            out.append("failures: none matched")
        if doc["text"]:
            out.append("---")
            out.append(doc["text"])
            out.append("---")
    digest_bytes = len(("\n".join(out)).encode("utf-8"))
    out.append("size: digest %d bytes of %d raw bytes; a derived view, not the run's record and not verification evidence"
               % (digest_bytes, raw["bytes"]))
    return "\n".join(out) + "\n"


def _by(fails):
    return ", ".join("%s=%d" % item for item in sorted(fails["by_recogniser"].items()))


def main(argv=None):
    parser = argparse.ArgumentParser(prog="output_digest")
    parser.add_argument("path")
    parser.add_argument("--exit-code", type=int)
    parser.add_argument("--max-bytes", type=int, default=DEFAULT_MAX_BYTES)
    parser.add_argument("--head", type=int, default=DEFAULT_HEAD)
    parser.add_argument("--tail", type=int, default=DEFAULT_TAIL)
    parser.add_argument("--ref")
    parser.add_argument("--duration", type=int)
    parser.add_argument("--withhold")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if not 256 <= args.max_bytes <= 1048576 or not 0 <= args.head <= 10000 or not 0 <= args.tail <= 10000:
        print("output-digest: --max-bytes is 256..1048576 and --head/--tail are 0..10000", file=sys.stderr)
        return 2
    try:
        doc = build(args.path, args.exit_code, args.max_bytes, args.head, args.tail,
                    args.ref, args.duration, args.withhold)
    except OSError as error:
        print("output-digest: cannot read the capture: %s" % error.strerror, file=sys.stderr)
        return 2
    if args.json:
        doc["size"] = {"raw_bytes": doc["raw"]["bytes"], "digest_text_bytes": len(doc["text"].encode("utf-8")),
                       "estimate": {"method": "bytes/4 (a heuristic, not a tokenizer)",
                                    "raw": doc["raw"]["bytes"] // 4,
                                    "digest_text": len(doc["text"].encode("utf-8")) // 4}}
        json.dump(doc, sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write("\n")
    else:
        sys.stdout.write(render(doc))
    return 0


if __name__ == "__main__":
    sys.exit(main())
