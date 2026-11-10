#!/usr/bin/env python3
"""Commit-level attribution ledger: which recorded attempt produced which commit.

`.oms/attribution.jsonl` (schema 1) is append-only and holds two row kinds:

  landing  written by patch-land after it applies an admitted patch: the base,
           the tree the patch produces, the changed paths, and the attempt,
           task, provider, model and room OMS already knew. patch-land never
           commits, so the commit does not exist yet.
  commit   written by `oms land` for each pushed commit that a landing row
           explains: the commit sha, tree, parents, `author_kind`
           (ai | human | mixed | unknown, the Agent Trace contributor-type
           vocabulary) and the landing id as evidence.

Nothing is guessed from the diff: `ai` needs the commit's tree to equal the
tree an admitted worker patch produced AND a recorded attempt; `mixed` means
the commit sits directly on that patch's base, touches its paths, but has a
different tree (the parent edited the patch before committing). Commits OMS
cannot tie to a landing get no row and read as `unrecorded`. No prompt text and
no diff content is stored. Writing is best effort: every writer subcommand
warns and exits 0, so a ledger problem never fails a landing.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

LIB = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(LIB))
LEDGER = os.path.join(".oms", "attribution.jsonl")
NOTES_REF = "refs/notes/oms-attribution"
MAX_COMMITS = 500
MAX_FILES = 500
KINDS = ("ai", "human", "mixed", "unknown")
ID = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")


class Failure(Exception):
    pass


def warn(message):
    print("warning: attribution: " + message, file=sys.stderr)


def run(argv, data=None, env=None):
    done = subprocess.run(argv, input=data, capture_output=True, check=False, env=env,
                          stdin=None if data is not None else subprocess.DEVNULL)
    return done.returncode, done.stdout, done.stderr


def git(repo, *args, data=None, env=None):
    rc, out, _ = run(["git", "-C", repo] + list(args), data=data, env=env)
    return out.decode("utf-8", "replace") if rc == 0 else None


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def ledger_path(state):
    return os.path.join(state, LEDGER)


def read_rows(state):
    path = ledger_path(state)
    if not os.path.lexists(path):
        return []
    rows = []
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                rows.append(row)
    return rows


def append_row(state, row):
    os.makedirs(os.path.join(state, ".oms"), exist_ok=True)
    data = (json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
    rc, _, err = run([sys.executable, os.path.join(LIB, "durable-jsonl.py"),
                      "--label", "attribution.jsonl", "append", ledger_path(state)], data=data)
    if rc != 0:
        raise Failure(err.decode("utf-8", "replace").strip() or "append failed")


def attempt_facts(state, attempt):
    """(recorded, provider, model) from the attempt projection and artifact index."""
    if not attempt or not ID.match(attempt):
        return False, "unknown", "unknown"
    rc, out, _ = run([os.path.join(ROOT, "scripts", "agent-events.sh"), "--repo", state,
                      "show", "--attempt", attempt, "--json"])
    try:
        shown = json.loads(out.decode("utf-8", "replace")) if rc == 0 else None
    except ValueError:
        shown = None
    if not isinstance(shown, dict) or shown.get("attempt_id") != attempt:
        return False, "unknown", "unknown"
    model = ""
    index = os.path.join(state, ".oms", "artifacts", "index.jsonl")
    try:
        with open(index, encoding="utf-8", errors="replace") as handle:
            for line in handle:
                if attempt not in line:
                    continue
                try:
                    row = json.loads(line)
                except ValueError:
                    continue
                if isinstance(row, dict) and row.get("attempt_id") == attempt:
                    model = row.get("selected_model") or row.get("requested_model") or model
    except OSError:
        pass
    return True, str(shown.get("provider") or "unknown"), str(model or "unknown")


def patch_tree(repo, base, patch):
    """Tree and paths a patch produces on `base`, from a private index (no worktree walk)."""
    tmp = tempfile.mkdtemp(prefix="oms-attribution.")
    try:
        env = dict(os.environ, GIT_INDEX_FILE=os.path.join(tmp, "index"))
        if git(repo, "read-tree", base, env=env) is None:
            return "", []
        rc, _, _ = run(["git", "-C", repo, "apply", "--cached", "--binary", patch], env=env)
        if rc != 0:
            return "", []
        tree = (git(repo, "write-tree", env=env) or "").strip()
        names = git(repo, "diff-index", "--cached", "--name-only", "-z", base, env=env) or ""
        return tree, [name for name in names.split("\0") if name]
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def cmd_landed(args):
    try:
        attempt = os.environ.get("OMS_ATTEMPT_ID", "")
        attempt = attempt if ID.match(attempt) else ""
        recorded, provider, model = attempt_facts(args.state_repo, attempt)
        tree, files = patch_tree(args.repo, args.base, args.patch)
        if not tree:
            warn("could not derive the patched tree; this landing cannot be matched to a commit")
        room = os.environ.get("OMS_ROOM_ID", "")
        append_row(args.state_repo, {
            "schema": 1, "kind": "landing", "ts": now(), "landing_id": args.landing_id,
            "base": args.base, "tree_after": tree, "files": files[:MAX_FILES],
            "files_changed": len(files), "patch_sha256": args.patch_sha,
            "attempt": attempt, "attempt_recorded": recorded, "task": args.task,
            "provider": provider, "model": model, "room": room if ID.match(room) else ""})
    except (Failure, OSError) as exc:
        warn("landing not recorded (%s); the landing itself is unaffected" % exc)
    return 0


def commit_files(repo, sha, parents):
    if parents:
        out = git(repo, "diff", "--name-only", "-z", parents[0], sha)
    else:
        out = git(repo, "diff-tree", "--root", "--no-commit-id", "--name-only", "-r", "-z", sha)
    return [name for name in (out or "").split("\0") if name]


def patch_id(repo, sha):
    diff = git(repo, "diff-tree", "-p", "--root", "--no-ext-diff", sha)
    if not diff:
        return ""
    rc, out, _ = run(["git", "-C", repo, "patch-id", "--stable"], data=diff.encode("utf-8"))
    return out.decode("utf-8", "replace").split(" ")[0].strip() if rc == 0 else ""


def cmd_commits(args):
    try:
        rows = read_rows(args.state_repo)
        recorded = {r.get("commit") for r in rows if r.get("kind") == "commit"}
        used = {(r.get("evidence") or {}).get("landing_id") for r in rows if r.get("kind") == "commit"}
        pending = [r for r in rows if r.get("kind") == "landing" and r.get("landing_id") not in used]
        if not pending:
            return 0
        listing = git(args.repo, "log", "--reverse", "--format=%H%x09%T%x09%P", args.range) or ""
        written = 0
        for line in listing.splitlines()[:MAX_COMMITS]:
            sha, tree, parents = (line.split("\t") + ["", ""])[:3]
            parents = parents.split()
            if sha in recorded:
                continue
            files = None
            for landing in pending:
                kind = ""
                if landing.get("tree_after") and landing["tree_after"] == tree:
                    kind = "ai"
                elif (landing.get("tree_after") and parents[:1] == [landing.get("base")]
                      and len(parents) == 1):
                    files = commit_files(args.repo, sha, parents) if files is None else files
                    if set(files) & set(landing.get("files") or []):
                        kind = "mixed"
                if not kind:
                    continue
                if not landing.get("attempt_recorded"):
                    kind = "unknown"
                files = commit_files(args.repo, sha, parents) if files is None else files
                append_row(args.state_repo, {
                    "schema": 1, "kind": "commit", "ts": now(), "commit": sha, "tree": tree,
                    "parents": parents, "author_kind": kind,
                    "provider": landing.get("provider") or "unknown",
                    "model": landing.get("model") or "unknown",
                    "attempt": landing.get("attempt", ""), "task": landing.get("task", ""),
                    "room": landing.get("room", ""), "files_changed": len(files),
                    "patch_id": patch_id(args.repo, sha),
                    "evidence": {"landing_id": landing["landing_id"], "land_receipt": args.receipt}})
                pending.remove(landing)
                written += 1
                break
        if written:
            print("attribution: recorded %d commit(s) in %s" % (written, LEDGER))
    except (Failure, OSError) as exc:
        warn("commits not recorded (%s); the landing itself is unaffected" % exc)
    return 0


def select_commits(repo, spec, path):
    if not spec or spec.startswith("-"):
        raise Failure("give a commit or a range such as BASE..HEAD")
    argv = ["rev-list", "--max-count=%d" % (MAX_COMMITS + 1)]
    if ".." not in spec and not path:
        argv = ["rev-list", "--max-count=1"]
    argv += [spec, "--"] + ([path] if path else [])
    out = git(repo, *argv)
    if out is None:
        raise Failure("%s does not name commits in this repository" % spec)
    return out.split()


def cmd_blame(args):
    try:
        rows = read_rows(args.state_repo)
        shas = select_commits(args.repo, args.spec, args.file)
    except (Failure, OSError) as exc:
        print("blame-session: %s" % exc, file=sys.stderr)
        return 2
    truncated = len(shas) > MAX_COMMITS
    shas = shas[:MAX_COMMITS]
    by_commit = {}
    for row in rows:
        if row.get("kind") == "commit" and row.get("commit") not in by_commit:
            by_commit[row["commit"]] = row
    by_patch = {}
    for row in by_commit.values():
        if row.get("patch_id"):
            by_patch.setdefault(row["patch_id"], row)
    result, summary = [], {kind: 0 for kind in KINDS}
    summary["unrecorded"] = 0
    moved = 0
    for sha in shas:
        row = by_commit.get(sha)
        entry = {"commit": sha, "status": "recorded"}
        if row is None and by_patch:
            original = by_patch.get(patch_id(args.repo, sha))
            if original is not None:
                row = original
                tree = (git(args.repo, "rev-parse", sha + "^{tree}") or "").strip()
                entry.update(status="moved", moved_from=original["commit"],
                             same_tree=tree == original.get("tree"))
                moved += 1
        if row is None:
            entry["status"] = "unrecorded"
            summary["unrecorded"] += 1
        else:
            for key in ("author_kind", "provider", "model", "attempt", "task", "room",
                        "files_changed", "evidence"):
                entry[key] = row.get(key, "")
            kind = row.get("author_kind") if row.get("author_kind") in KINDS else "unknown"
            summary[kind] += 1
        result.append(entry)
    exported = None
    if args.export_notes:
        exported = 0
        for entry in result:
            if entry["status"] != "recorded":
                continue
            payload = json.dumps(by_commit[entry["commit"]], ensure_ascii=False, sort_keys=True)
            if git(args.repo, "notes", "--ref", NOTES_REF, "add", "-f", "-m", payload,
                   entry["commit"]) is None:
                warn("could not write a note for %s" % entry["commit"][:12])
            else:
                exported += 1
    if args.json:
        doc = {"schema": 1, "range": args.spec, "commits": result, "summary": dict(summary, moved=moved),
               "truncated": truncated}
        if exported is not None:
            doc["exported_notes"] = exported
        print(json.dumps(doc, ensure_ascii=False, indent=2))
    else:
        for entry in result:
            if entry["status"] == "unrecorded":
                print("%s  unrecorded" % entry["commit"][:12])
                continue
            moved_note = " moved from %s" % entry["moved_from"][:12] if entry["status"] == "moved" else ""
            print("%s  %s%s  %s/%s  attempt=%s task=%s room=%s files=%s evidence=%s" % (
                entry["commit"][:12], entry["author_kind"], moved_note, entry["provider"],
                entry["model"], entry["attempt"] or "-", entry["task"] or "-", entry["room"] or "-",
                entry["files_changed"], (entry["evidence"] or {}).get("landing_id", "-")))
        print("summary: %d ai / %d human / %d mixed / %d unknown / %d unrecorded%s%s" % (
            summary["ai"], summary["human"], summary["mixed"], summary["unknown"],
            summary["unrecorded"], " (%d moved)" % moved if moved else "",
            " (first %d commits only)" % MAX_COMMITS if truncated else ""))
        if exported is not None:
            print("exported %d note(s) to %s (local ref; OMS never pushes it)" % (exported, NOTES_REF))
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(prog="attribution.py")
    sub = parser.add_subparsers(dest="action", required=True)
    for name in ("landed", "commits", "blame"):
        item = sub.add_parser(name)
        item.add_argument("--repo", default=os.getcwd())
        item.add_argument("--state-repo")
        if name == "landed":
            item.add_argument("--landing-id", required=True)
            item.add_argument("--base", required=True)
            item.add_argument("--patch", required=True)
            item.add_argument("--patch-sha", default="")
            item.add_argument("--task", default="")
        elif name == "commits":
            item.add_argument("--range", required=True)
            item.add_argument("--receipt", default="")
        else:
            item.add_argument("spec", metavar="COMMIT_OR_RANGE")
            item.add_argument("--file", default="")
            item.add_argument("--json", action="store_true")
            item.add_argument("--export-notes", action="store_true")
    args = parser.parse_args(argv)
    top = git(args.repo, "rev-parse", "--show-toplevel")
    if top is None:
        print("attribution: not a git checkout: %s" % args.repo, file=sys.stderr)
        return 0 if args.action != "blame" else 2
    args.repo = top.strip()
    args.state_repo = args.state_repo or args.repo
    return {"landed": cmd_landed, "commits": cmd_commits, "blame": cmd_blame}[args.action](args)


if __name__ == "__main__":
    sys.exit(main())
