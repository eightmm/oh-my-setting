#!/usr/bin/env python3
"""Carry a project memory note beyond its clone, and read other clones' notes.

Project memory lives in a clone-local, git-ignored `.oms/`, so a lesson stays
where it was written. Three paths out, each explicit:

promote   copy one note into the person's global harness memory. A dry run
          unless --apply; parent only; the text passes the strict
          sensitive scan after machine paths are normalized, and the copy
          names the event it came from but not the repository or its path.
graduate  print, as a diff, how the note would read in a checked-in document
          (AGENTS.md or PROJECT.md). It never writes: the edit goes through
          the ordinary edit and review path, and the note is retired by the
          person afterwards.
recall-all  rank a query against the derived index of every repository the
          maintenance tick knows. Read-only: an index is read as last
          synchronized, nothing is rebuilt and no recall receipt is written.
"""

import argparse
import difflib
import json
import os
import pathlib
import re
import sys
import tempfile

LIB = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(LIB))
import agent_memory_review as review  # noqa: E402  (the shared store reader and scan)

SCHEMA = 1
MAX_NOTE = 2000
MAX_BULLET = 400
MAX_REPOS = 30
MAX_HITS = 50
GRADUATE_TARGETS = ("AGENTS.md", "PROJECT.md")
DECISIONS_RE = re.compile(r"^##\s+(decisions|결정|의사결정)\s*$", re.IGNORECASE)


def die(message, code=2):
    sys.stderr.write("error: %s\n" % message)
    raise SystemExit(code)


def parent_only(what):
    if os.environ.get("OMS_HARNESS_CHILD") == "1":
        die("a harness child cannot %s; return the request to the parent agent" % what)


def find_note(store, ident):
    if not review.ID_RE.fullmatch(ident or ""):
        die("--id needs a memory event ID (see: oms agent-memory search)")
    notes, _status = store.notes()
    matches = [entry for entry in notes if entry.event_id == ident]
    if len(matches) != 1:
        die("no active shared note has the ID %s (retired and superseded notes cannot be carried over)" % ident)
    body = matches[0].body.strip()
    if not body:
        die("the note %s has no text" % ident)
    if len(body) > MAX_NOTE:
        die("the note %s is longer than %d characters; supersede it with a shorter one first" % (ident, MAX_NOTE))
    return matches[0], body


def portable(text):
    """TEXT with home prefixes normalized, or None when the strict scan still flags it or cannot run."""
    done = review.library("agent_memory_normalize_machine_paths", stdin=text + "\n")
    if done.returncode != 0:
        return None
    clean = done.stdout.rstrip("\n")
    return clean if review.scan([clean]) == [True] else None


def global_file():
    done = review.library("agent_memory_global_file")
    path = done.stdout.strip()
    if done.returncode != 0 or not path:
        die("could not resolve the global memory file")
    return path


def run_promote(store, args):
    parent_only("write global memory")
    entry, body = find_note(store, args.id)
    text = portable(body)
    if text is None:
        die("the note still looks sensitive or machine-specific after its paths were normalized; "
            "supersede it with a portable wording first", 3)
    name = os.path.basename(store.repo)
    mentions = bool(name) and len(name) >= 3 and name.casefold() in text.casefold()
    target = global_file()
    stamped = "%s\n(promoted from project note %s)" % (text, entry.event_id)
    already = False
    try:
        with open(target, encoding="utf-8", errors="replace") as handle:
            already = ("(promoted from project note %s)" % entry.event_id) in handle.read()
    except OSError:
        pass
    result = {"schema": SCHEMA, "kind": "oms-memory-promote", "id": entry.event_id, "applied": False,
              "already_promoted": already, "mentions_repository_name": mentions, "bytes": len(text.encode("utf-8"))}
    if already:
        result_line = "memory: note %s is already in global memory; nothing to do" % entry.event_id
    elif not args.apply:
        result_line = "memory: dry run; nothing changed. Re-run with --apply to copy the note to global memory."
    else:
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", suffix=".note", delete=False) as handle:
            handle.write(stamped + "\n")
            note = handle.name
        try:
            done = review.library('agent_memory_append_file "$1" global "$2" "$3" promoted', target, args.agent, note)
        finally:
            os.unlink(note)
        if done.returncode != 0:
            die((done.stderr.strip().splitlines() or ["global memory append failed"])[-1].replace("error: ", ""),
                3 if done.returncode == 3 else 2)
        result["applied"] = True
        result_line = "memory: promoted %s to global memory; the project note is unchanged" % entry.event_id
    if args.json:
        print(json.dumps(result, ensure_ascii=False, sort_keys=True))
        return 0
    if not already:
        print("memory: would promote" if not args.apply else "memory: promoting", "%s (%d bytes):" %
              (entry.event_id, result["bytes"]))
        for line in text.splitlines():
            print("    " + line)
        if mentions:
            print("note: the text names this repository; global memory is read in every project")
    print(result_line)
    return 0


def bullet(text):
    line = " ".join(text.split())
    if len(line) > MAX_BULLET:
        die("the note is longer than %d characters on one line; supersede it with a shorter one first" % MAX_BULLET)
    return "- " + line


def graduated(lines, name, item):
    """LINES with ITEM added: under PROJECT.md's Decisions heading (created when absent), else at the end."""
    out = list(lines)
    if name == "PROJECT.md":
        start = next((i for i, line in enumerate(out) if DECISIONS_RE.match(line.strip())), None)
        if start is None:
            while out and not out[-1].strip():
                out.pop()
            return out + ([""] if out else []) + ["## Decisions", "", item]
        end = next((i for i in range(start + 1, len(out)) if out[i].startswith("## ")), len(out))
        while end > start + 1 and not out[end - 1].strip():
            end -= 1
        return out[:end] + [item] + out[end:]
    while out and not out[-1].strip():
        out.pop()
    return out + [item]


def run_graduate(store, args):
    if args.to not in GRADUATE_TARGETS:
        die("--to takes %s" % " or ".join(GRADUATE_TARGETS))
    entry, body = find_note(store, args.id)
    text = portable(body)
    if text is None:
        die("the note looks sensitive or machine-specific; a checked-in document must not carry it", 3)
    path = os.path.join(store.repo, args.to)
    if os.path.islink(path) or not os.path.isfile(path):
        die("%s is not a regular file at the repository root" % args.to)
    try:
        with open(path, encoding="utf-8") as handle:
            before = handle.read().split("\n")
    except (OSError, UnicodeError):
        die("%s could not be read as UTF-8" % args.to)
    if before and before[-1] == "":
        before.pop()
    item = bullet(text)
    if item in before:
        print("memory: %s already carries this line; retire the note with: "
              "oms agent-memory retire --id %s --reason graduated" % (args.to, entry.event_id))
        return 0
    after = graduated(before, args.to, item)
    diff = list(difflib.unified_diff(before, after, "a/" + args.to, "b/" + args.to, lineterm="", n=2))
    if args.json:
        print(json.dumps({"schema": SCHEMA, "kind": "oms-memory-graduate", "id": entry.event_id, "to": args.to,
                          "line": item, "diff": diff, "applied": False}, ensure_ascii=False, sort_keys=True))
        return 0
    print("\n".join(diff))
    print("memory: preview only; %s was not changed. Make the edit through your usual review path, then:" % args.to)
    print("    oms agent-memory retire --id %s --reason graduated" % entry.event_id)
    return 0


def registry():
    path = os.environ.get("OMS_TICK_REGISTRY") or os.path.join(
        os.environ.get("XDG_CONFIG_HOME") or os.path.join(os.path.expanduser("~"), ".config"),
        "oh-my-setting", "tick-repos.txt")
    try:
        with open(path, encoding="utf-8") as handle:
            rows = [line.strip() for line in handle if line.strip() and not line.startswith("#")]
    except OSError:
        return None
    return list(dict.fromkeys(rows))


def tilde(path):
    home = os.path.expanduser("~")
    return "~" + path[len(home):] if home and (path == home or path.startswith(home + os.sep)) else path


def run_recall_all(args):
    parent_only("read other repositories' memory")
    if not args.text:
        die("recall --all-repos requires --text QUERY (or a positional query)")
    repos = registry()
    if repos is None:
        die("no repository registry (register a repository with: oms tick register)")
    index = review.load_index()
    event_at, kind_at, body_at = (index.ENTRY_FIELDS.index(name) for name in ("event_id", "kind", "body"))
    when_at, agent_at = index.ENTRY_FIELDS.index("occurred_at"), index.ENTRY_FIELDS.index("agent")
    hits, skipped, unread = [], 0, []
    for repo in repos[:MAX_REPOS]:
        memory = os.path.join(repo, ".oms", "memory")
        database, shared = os.path.join(memory, "memory.sqlite3"), os.path.join(memory, "shared.md")
        if not os.path.isfile(database) or os.path.islink(database):
            skipped += 1
            continue
        try:
            _notes, _pins, status = index.memory_state(shared, os.path.join(memory, "pins.md"))
            db = index.connect_readonly(database)
            try:
                rows = index.ranked_recall(db, args.text, "", 0)  # 0: the pool; no recall receipt is written
            finally:
                db.close()
        except Exception:  # one unreadable clone must not hide the others
            unread.append(tilde(repo))
            continue
        rows = index.filter_memory_state(rows, status, False)[:args.limit]
        for row in rows:
            hits.append({"repo": tilde(repo), "event_id": str(row[event_at]), "kind": str(row[kind_at]),
                         "occurred_at": str(row[when_at]), "agent": str(row[agent_at]),
                         "body": str(row[body_at]).strip()})
    hits = hits[:MAX_HITS]
    cleared = review.scan([hit["body"] for hit in hits])
    for hit, clear in zip(hits, cleared):
        if not clear:
            hit["body"], hit["withheld"] = "", True
    if args.json:
        print(json.dumps({"schema": SCHEMA, "kind": "oms-memory-recall-all", "query": args.text, "hits": hits,
                          "repositories": {"registered": len(repos), "read": min(len(repos), MAX_REPOS) - skipped - len(unread),
                                           "without_index": skipped, "unread": unread,
                                           "not_reached": max(0, len(repos) - MAX_REPOS)}},
                         ensure_ascii=False, sort_keys=True))
        return 0 if hits else 1
    current = None
    for hit in hits:
        if hit["repo"] != current:
            current = hit["repo"]
            print("%s" % current)
        first = (hit["body"].splitlines() or [""])[0]
        print("  %s  %s  %s" % (hit["occurred_at"], hit["event_id"],
                                "[withheld: sensitive-looking content]" if hit.get("withheld") else first[:200]))
    sys.stderr.write('memory: %d recalled entr%s for "%s" across %d registered repositor%s (%d without an index%s); '
                     "each index is read as last synchronized and nothing was written\n" % (
                         len(hits), "y" if len(hits) == 1 else "ies", args.text, len(repos),
                         "y" if len(repos) == 1 else "ies", skipped,
                         "; unread: " + ", ".join(unread) if unread else ""))
    if len(repos) > MAX_REPOS:
        sys.stderr.write("memory: incomplete: only the first %d registered repositories were read\n" % MAX_REPOS)
    return 0 if hits else 1


def main():
    parser = argparse.ArgumentParser(prog="agent-memory", add_help=False)
    parser.add_argument("command", choices=("promote", "graduate", "recall-all"))
    parser.add_argument("query", nargs="*", default=[])
    parser.add_argument("--repo", default=os.environ.get("OMS_STATE_REPO") or os.getcwd())
    parser.add_argument("--agent", default="")
    parser.add_argument("--id", default="")
    parser.add_argument("--to", default="")
    parser.add_argument("--text", default="")
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--global", dest="to_global", action="store_true")
    parser.add_argument("--all-repos", action="store_true")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--json", action="store_true")
    args, extra = parser.parse_known_args()
    if extra:
        die("%s does not take %s" % (args.command, extra[0]))
    args.text = args.text or " ".join(args.query)
    if args.command == "recall-all":
        if args.limit < 1 or args.limit > 20:
            die("--limit is 1 to 20 notes per repository with --all-repos")
        if args.id or args.to or args.apply:
            die("--id/--to/--apply do not apply to recall --all-repos")
        return run_recall_all(args)
    if args.text or args.all_repos:
        die("%s takes --id, not a query" % args.command)
    if not os.path.isdir(args.repo):
        die("--repo %s is not a directory" % args.repo)
    store = review.Store(args.repo)
    if not args.agent:
        args.agent = (review.library("oms_detect_agent").stdout.strip() or "agent")
    if args.command == "promote":
        if not args.to_global or args.to:
            die("promote copies a project note to global memory: promote --id EVENT --global [--apply]")
        return run_promote(store, args)
    if args.to_global or args.apply:
        die("graduate only previews a diff: graduate --id EVENT --to AGENTS.md|PROJECT.md")
    return run_graduate(store, args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        sys.stderr.write("error: memory: %s\n" % error)
        raise SystemExit(2)
