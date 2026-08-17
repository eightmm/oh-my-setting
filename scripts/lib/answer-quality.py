#!/usr/bin/env python3
"""Classify whether a provider artifact contains a usable answer."""

import re
import sys


NOISE = re.compile(
    r"^\s*(model-route:|model-result:|tokens used|\[REDACTED|OpenAI Codex|"
    r"workdir:|model:|provider:|approval:|sandbox:|reasoning effort:|"
    r"reasoning summaries:|session id:|-{3,}$|user$|DRY RUN)"
)
REFUSAL = re.compile(
    r"^(jetski: no output produced\b"
    r"|add an allow-rule\b"
    r"|alternatively, re-run with\b"
    r"|.*\bcommand not found\b"
    r"|(error|fatal)[: ].*\b(permission|not authenticated|not logged in|"
    r"unauthorized|invalid api key|quota|rate limit)\b)",
    re.IGNORECASE,
)


STOP_REASON = re.compile(
    r"^stop-reason: provider=\S+ reason=(?P<reason>\S+) subtype=(?P<subtype>\S+)"
    r" is_error=(?P<is_error>[01])$"
)


def main(path: str) -> None:
    with open(path, encoding="utf-8", errors="replace") as handle:
        raw_lines = [line.rstrip() for line in handle]

    # A recorded stop reason outranks every text heuristic below: the
    # transport said WHY the model stopped, and a max_tokens stop is a
    # truncation however finished the sentences look. Read it only from the
    # transport's own region — after the LAST "## Output" heading — because a
    # quoted prompt, a diff's context lines, or a replayed thread turn can
    # carry a marker line of its own (the same reason the MCP extractor
    # anchors on the last section). Whole-file only when no heading exists.
    scan_from = 0
    for index, line in enumerate(raw_lines):
        if line == "## Output":
            scan_from = index + 1
    for line in raw_lines[scan_from:]:
        match = STOP_REASON.match(line.strip())
        if not match:
            continue
        if match.group("is_error") == "1":
            print("blocked")
            return
        if match.group("reason") == "max_tokens":
            print("truncated")
            return
        break

    lines = [
        line.strip()
        for line in raw_lines
        if line.strip()
        and not NOISE.match(line)
        and not STOP_REASON.match(line.strip())
    ]

    if not lines:
        print("empty")
        return
    if all(REFUSAL.match(line) for line in lines):
        print("blocked")
        return

    body = "\n".join(lines)
    if len(body.encode("utf-8")) < 24:
        print("empty")
    elif all(line.endswith("?") for line in lines):
        print("thin")
    else:
        print("ok")


if __name__ == "__main__":
    main(sys.argv[1])
