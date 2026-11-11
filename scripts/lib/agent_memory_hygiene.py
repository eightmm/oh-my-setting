#!/usr/bin/env python3
"""Read-only hygiene report for one project's memory (`oms agent-memory hygiene`).

Three questions, answered from files that already exist, with no model call:

  cold        which active notes were never recalled, or not recalled for a long time
              (the recall receipts `recall` already keeps in the derived index);
  duplicated  which active notes or pins already read the same in an always-loaded
              instruction file (AGENTS.md, CLAUDE.md, ...), so they cost tokens twice;
  index caps  whether a provider's auto-loaded memory index is longer than the part
              the provider loads, so its tail is silently ignored.

The report never deletes, edits, retires or synchronizes anything: the index is opened
read-only, the memory logs are only read, and a person decides what to retire. A note
listed here is a candidate, not a verdict: being cold or duplicated does not make it
wrong, and recall receipts only exist for what `recall` returned.
"""
import argparse
import datetime
import json
import os
import pathlib
import re
import sqlite3
import sys
import time

LIB = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(LIB))
import agent_memory_review as review  # noqa: E402  (the shared store reader)
import agent_memory_sources as sources  # noqa: E402  (path naming and the Claude auto-memory location)

SCHEMA = 1
COLD_DAYS_DEFAULT = 30
LIST_DEFAULT = 20
MAX_READ = 4 * 1024 * 1024
MAX_INSTRUCTION = 1024 * 1024
MIN_EXACT_CHARS = 20
MIN_NEAR_WORDS = 8
SHINGLE = 5
NEAR_RATIO = 0.9
NEAR_CAP_RATIO = 0.9

# The Claude Code memory page documents that the first 200 lines or the first 25KB of the
# auto-memory index MEMORY.md, whichever comes first, load at session start and that
# content beyond that is not loaded. The page does not say whether KB is 1000 or 1024
# bytes, so the smaller figure is used: a file between the two is reported, never hidden.
# Overridable per environment; 0 turns one dimension off.
CLAUDE_CAP_LINES = 200
CLAUDE_CAP_BYTES = 25000
CLAUDE_CAP_URL = "https://code.claude.com/docs/en/memory"
CAP_LINES_ENV = "OMS_MEMORY_INDEX_CAP_LINES"
CAP_BYTES_ENV = "OMS_MEMORY_INDEX_CAP_BYTES"

IMPORT_RE = re.compile(r"(?<![\w@])@([A-Za-z0-9._~/-][A-Za-z0-9._/~-]*)")
LEADING_RE = re.compile(r"^\s*(?:#{1,6}\s+|>+\s*|[-*+]\s+|\d+[.)]\s+)")


def die(message, code=2):
    sys.stderr.write("error: %s\n" % message)
    raise SystemExit(code)


def normalize(text):
    """Markdown-light, case-folded, single-spaced text: wrapped lines and bullets compare equal."""
    lines = []
    for line in text.splitlines():
        line = LEADING_RE.sub("", line)
        lines.append(line.replace("`", "").replace("*", ""))
    return " ".join(" ".join(lines).split()).casefold()


def shingles(text):
    words = text.split()
    return {" ".join(words[i:i + SHINGLE]) for i in range(len(words) - SHINGLE + 1)}


def read_text(path, limit):
    try:
        with open(path, "rb") as handle:
            return handle.read(limit).decode("utf-8", errors="replace")
    except OSError:
        return None


# ------------------------------------------------------------- cold notes

def recall_receipts(index, db_path):
    """(state, {event_id: (last_accessed, hit_count)}); the index is opened read-only."""
    if not os.path.isfile(db_path):
        return "absent", {}
    try:
        db = index.connect_readonly(db_path)
        try:
            rows = db.execute("select event_id, last_accessed, hit_count from memory_access").fetchall()
        finally:
            db.close()
    except (sqlite3.Error, OSError):
        return "unreadable", {}
    return "present", {row[0]: (row[1], row[2]) for row in rows}


def cold_notes(index, notes, receipts, cold_days, now):
    found = []
    limit_s = cold_days * 86400
    for entry in notes:
        born = index.parse_ts(entry.occurred_at)
        if born is None or now - born < limit_s:
            continue
        last, hits = receipts.get(entry.event_id, (None, 0))
        seen = index.parse_ts(last) if last else None
        if not hits:
            reason = "never"
        elif seen is not None and now - seen >= limit_s:
            reason = "long-ago"
        else:
            continue
        found.append({"id": entry.event_id, "source": entry.source, "kind": entry.kind, "reason": reason,
                      "age_days": int((now - born) // 86400), "hits": int(hits or 0),
                      "last_recalled": last if hits else None,
                      "preview": review.first_line(entry.body, 80)})
    found.sort(key=lambda row: (-row["age_days"], row["id"]))
    return found


# ------------------------------------------------------ duplicated in instructions

def instruction_files(repo):
    """Always-loaded instruction files: the repository's and the user's, plus one level of @imports."""
    home = sources.home_dir()
    codex = os.environ.get("CODEX_HOME") or os.path.join(home, ".codex")
    gemini = os.path.join(home, ".gemini")
    wanted = [os.path.join(repo, name) for name in
              ("AGENTS.md", "AGENTS.override.md", "CLAUDE.md", os.path.join(".claude", "CLAUDE.md"),
               "CLAUDE.local.md", "GEMINI.md")]
    wanted += [os.path.join(home, ".claude", "CLAUDE.md"), os.path.join(codex, "AGENTS.md"),
               os.path.join(gemini, "AGENTS.md"), os.path.join(gemini, "GEMINI.md")]
    found, seen = [], set()
    for path in wanted:
        text = read_text(path, MAX_INSTRUCTION) if os.path.isfile(path) else None
        real = os.path.realpath(path)
        if text is None or real in seen:
            continue
        seen.add(real)
        found.append((path, text))
        for target in IMPORT_RE.findall(text):
            candidate = os.path.realpath(os.path.join(os.path.dirname(path), os.path.expanduser(target)))
            if candidate in seen or not os.path.isfile(candidate):
                continue
            if not (candidate == repo or candidate.startswith(repo.rstrip("/") + "/")):
                continue  # an import outside the repository is not this project's rule file
            imported = read_text(candidate, MAX_INSTRUCTION)
            if imported is not None:
                seen.add(candidate)
                found.append((candidate, imported))
    return found


def duplicated_notes(notes, files, repo):
    home = sources.home_dir()
    prepared = []
    for path, text in files:
        flat = normalize(text)
        prepared.append((sources.show(path, repo, home), flat, shingles(flat)))
    found = []
    for entry in notes:
        flat = normalize(entry.body)
        if len(flat) < MIN_EXACT_CHARS:
            continue
        words = len(flat.split())
        mine = shingles(flat) if words >= MIN_NEAR_WORDS else set()
        best = None
        for name, text, theirs in prepared:
            if flat in text:
                best = ("exact", name, 1.0)
                break
            if mine:
                ratio = len(mine & theirs) / len(mine)
                if ratio >= NEAR_RATIO and (best is None or ratio > best[2]):
                    best = ("near", name, round(ratio, 2))
        if best:
            found.append({"id": entry.event_id, "source": entry.source, "kind": entry.kind,
                          "match": best[0], "file": best[1], "overlap": best[2],
                          "bytes": len(entry.body.encode("utf-8")),
                          "preview": review.first_line(entry.body, 80)})
    found.sort(key=lambda row: (row["match"] != "exact", -row["bytes"], row["id"]))
    return found


# ------------------------------------------------------------- index caps

def cap_value(name, default):
    raw = os.environ.get(name)
    if raw is None or raw == "":
        return default
    if not raw.isdigit():
        die("%s must be a whole number (0 turns that check off), not %r" % (name, raw))
    return int(raw)


def index_caps(repo):
    home = sources.home_dir()
    path = os.path.join(home, ".claude", "projects", sources.claude_project_key(repo), "memory", "MEMORY.md")
    cap_lines = cap_value(CAP_LINES_ENV, CLAUDE_CAP_LINES)
    cap_bytes = cap_value(CAP_BYTES_ENV, CLAUDE_CAP_BYTES)
    if not os.path.isfile(path):
        return []
    try:
        size = os.stat(path).st_size
    except OSError:
        return []
    text = read_text(path, MAX_READ) or ""
    rows = text.splitlines(keepends=True)
    lines, loaded_lines, used = len(rows), 0, 0
    for row in rows:
        width = len(row.encode("utf-8"))
        if (cap_lines and loaded_lines >= cap_lines) or (cap_bytes and used + width > cap_bytes):
            break
        loaded_lines += 1
        used += width
    over = bool((cap_lines and lines > cap_lines) or (cap_bytes and size > cap_bytes))
    near = bool((cap_lines and lines >= cap_lines * NEAR_CAP_RATIO) or (cap_bytes and size >= cap_bytes * NEAR_CAP_RATIO))
    return [{"provider": "claude", "role": "auto-memory index", "path": sources.show(path, repo, home),
             "lines": lines, "bytes": size, "cap_lines": cap_lines or None, "cap_bytes": cap_bytes or None,
             "status": "over" if over else ("near" if near else "ok"),
             "lines_not_loaded": lines - loaded_lines if over else 0,
             "cap_source": CLAUDE_CAP_URL,
             "cap_overridden": bool(os.environ.get(CAP_LINES_ENV) or os.environ.get(CAP_BYTES_ENV))}]


# ------------------------------------------------------------------ report

def build(repo, cold_days):
    store = review.Store(repo)
    index = review.load_index()
    if os.path.isfile(store.shared) or os.path.isfile(store.pins):
        shared, pinned, status = index.memory_state(store.shared, store.pins)
    else:
        shared, pinned, status = [], [], {}
    notes = [e for e in shared if e.kind != "retirement" and e.event_id not in status]
    pins = [e for e in pinned if e.event_id not in status]
    state, receipts = recall_receipts(index, os.path.join(store.dir, "memory.sqlite3"))
    now = time.time()
    cold = cold_notes(index, notes, receipts, cold_days, now) if state == "present" else []
    files = instruction_files(store.repo)
    dup = duplicated_notes(notes + pins, files, store.repo)
    return {"schema": SCHEMA, "kind": "oms-memory-hygiene", "repo": os.path.basename(store.repo),
            "read_only": True, "cold_days": cold_days,
            "active": {"notes": len(notes), "pins": len(pins)},
            "cold": {"recall_receipts": state, "count": len(cold), "items": cold},
            "duplicated": {"instruction_files": [sources.show(p, store.repo, sources.home_dir()) for p, _ in files],
                           "count": len(dup), "items": dup},
            "index_caps": index_caps(store.repo),
            "not_checked": ["codex memories and antigravity rules: no documented load cap was confirmed, so none is assumed"]}


def clip(report, limit):
    for key in ("cold", "duplicated"):
        items = report[key]["items"]
        report[key]["truncated"] = len(items) > limit
        report[key]["items"] = items[:limit]
    return report


def render(report):
    out = ["memory hygiene for %s: read-only; no note is changed (retire or supersede one yourself)" % report["repo"]]
    cold, dup = report["cold"], report["duplicated"]
    out.append("active: %d note(s), %d pin(s)" % (report["active"]["notes"], report["active"]["pins"]))
    if cold["recall_receipts"] != "present":
        out.append("cold notes: not assessed; recall receipts are %s (they appear after `oms agent-memory recall`)"
                   % ("absent: no index yet" if cold["recall_receipts"] == "absent" else "unreadable"))
    else:
        out.append("cold notes: %d never recalled or not recalled for %d+ days (receipts count hits since this index was last rebuilt)"
                   % (cold["count"], report["cold_days"]))
        for row in cold["items"]:
            when = "never recalled" if row["reason"] == "never" else "last recalled %s" % row["last_recalled"]
            out.append("  %s  %dd old, %d hit(s), %s  %s" % (row["id"], row["age_days"], row["hits"], when, row["preview"]))
        if cold["truncated"]:
            out.append("  ... more; raise --limit")
    out.append("duplicated in always-loaded instructions: %d (%s)" % (
        dup["count"], ", ".join(dup["instruction_files"]) or "no instruction file found"))
    for row in dup["items"]:
        out.append("  %s  %s in %s  %s" % (row["id"], row["match"], row["file"], row["preview"]))
    if dup["truncated"]:
        out.append("  ... more; raise --limit")
    if not report["index_caps"]:
        out.append("provider index caps: no Claude auto-memory index found for this repository")
    for row in report["index_caps"]:
        cap = " / ".join(part for part in (
            "%d lines" % row["cap_lines"] if row["cap_lines"] else "",
            "%d bytes" % row["cap_bytes"] if row["cap_bytes"] else "") if part) or "no cap set"
        line = "provider index caps: %s %s (%s): %d lines, %d bytes; load cap %s -> %s" % (
            row["provider"], row["role"], row["path"], row["lines"], row["bytes"], cap, row["status"])
        if row["status"] == "over":
            line += "; about %d line(s) at the end are not loaded at session start" % row["lines_not_loaded"]
        out.append(line)
        out.append("  cap source: %s%s" % (row["cap_source"], " (overridden by environment)" if row["cap_overridden"] else ""))
    out.append("not checked: " + "; ".join(report["not_checked"]))
    return "\n".join(out)


def main():
    parser = argparse.ArgumentParser(prog="agent-memory hygiene", add_help=False)
    parser.add_argument("command", choices=("hygiene",))
    parser.add_argument("--repo", default=os.environ.get("OMS_STATE_REPO") or os.getcwd())
    parser.add_argument("--cold-days", default=str(COLD_DAYS_DEFAULT))
    parser.add_argument("--limit", default=str(LIST_DEFAULT))
    parser.add_argument("--json", action="store_true")
    args, extra = parser.parse_known_args()
    if extra:
        die("hygiene does not take %s (project memory only; --global and --file are not supported)" % extra[0])
    for name, value in (("--cold-days", args.cold_days), ("--limit", args.limit)):
        if not value.isdigit() or int(value) < 1:
            die("%s requires a positive integer" % name)
    if not os.path.isdir(args.repo):
        die("--repo %s is not a directory" % args.repo)
    report = clip(build(args.repo, int(args.cold_days)), int(args.limit))
    if args.json:
        json.dump(report, sys.stdout, ensure_ascii=False, sort_keys=True)
        sys.stdout.write("\n")
    else:
        print(render(report))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        sys.stderr.write("error: memory hygiene: %s\n" % error)
        raise SystemExit(2)
