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


def main(path: str) -> None:
    with open(path, encoding="utf-8", errors="replace") as handle:
        lines = [
            line.rstrip().strip()
            for line in handle
            if line.strip() and not NOISE.match(line.rstrip())
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
