"""Session handoff digests as recall pointers.

A handoff digest already holds what a finished session was asked and how it
ended, but recall never looked at it and the resume hook names only the newest
one. This reads the head of each digest still on disk at recall time and
returns pointers — which digest, when, one line of what it was about, and the
command that opens it — never the digest body. Nothing is indexed or stored:
a digest `oms gc` removed simply stops matching.
"""

import os
import re

MAX_FILES = 200
HEAD_BYTES = 8192
MAX_POINTERS = 3
EXCERPT = 160
TOKEN_RE = re.compile(r"[^\W_]+", re.UNICODE)
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,200}\.md$")
# Header lines that describe the machine, not the work.
SKIP_PREFIXES = ("- cwd:", "- source:", "- verification_observed_at:", "- assistant_summary:")
REQUEST_HEADING = "## Original request"


def _digests(directory):
    """(mtime, name, path) for the newest regular digest files, newest first."""
    found = []
    try:
        with os.scandir(directory) as entries:
            for entry in entries:
                if not NAME_RE.match(entry.name):
                    continue
                try:
                    if entry.is_symlink() or not entry.is_file(follow_symlinks=False):
                        continue
                    found.append((entry.stat(follow_symlinks=False).st_mtime, entry.name, entry.path))
                except OSError:
                    continue
    except OSError:
        return []
    found.sort(reverse=True)
    return found[:MAX_FILES]


def _head(path):
    try:
        with open(path, "rb") as handle:
            return handle.read(HEAD_BYTES).decode("utf-8", errors="replace")
    except OSError:
        return ""


def _fields(text):
    """(captured, agent, searchable text, first line of the original request)."""
    captured = agent = request = ""
    kept, in_request = [], False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("- captured:"):
            captured = stripped[len("- captured:"):].strip()[:40]
        elif stripped.startswith("- agent:"):
            agent = stripped[len("- agent:"):].strip()[:40]
        if stripped.startswith(SKIP_PREFIXES):
            continue
        if stripped.startswith("## "):
            in_request = stripped.startswith(REQUEST_HEADING)
        elif in_request and stripped and not request:
            request = " ".join(stripped.split())[:EXCERPT]
        kept.append(line)
    return captured, agent, "\n".join(kept), request


def pointers(directory, query, clear=None, limit=MAX_POINTERS):
    """Up to LIMIT pointers to digests in DIRECTORY whose head matches QUERY.

    CLEAR takes a list of excerpts and returns the set the secret scan passes;
    an excerpt it does not pass is withheld while the pointer stays."""
    tokens = list(dict.fromkeys(token.casefold() for token in TOKEN_RE.findall(query or "")))
    if not tokens or not directory:
        return []
    ranked = []
    for mtime, name, path in _digests(directory):
        captured, agent, text, request = _fields(_head(path))
        folded = text.casefold()
        score = sum(folded.count(token) for token in tokens)
        if score:
            ranked.append((score, mtime, name, captured, agent, request))
    ranked.sort(reverse=True)
    chosen = ranked[:max(0, limit)]
    passed = clear([row[5] for row in chosen if row[5]]) if clear and chosen else set()
    rows = []
    for _score, _mtime, name, captured, agent, request in chosen:
        withheld = bool(request) and request not in passed
        rows.append({
            "event_id": "handoff:" + name,
            "source": "handoffs",
            "kind": "handoff",
            "occurred_at": captured,
            "agent": agent,
            "body": "" if withheld else request,
            "withheld": withheld,
            "open": "oms session-handoff show " + name,
        })
    return rows


def render(row):
    about = "[withheld: sensitive-looking content]" if row["withheld"] else (row["body"] or "(no request recorded)")
    return "## %s %s\n\nhandoff digest: %s\nopen: %s\n" % (
        row["occurred_at"] or "unknown-time", row["agent"] or "agent", about, row["open"])
