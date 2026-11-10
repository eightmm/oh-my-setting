#!/usr/bin/env python3
"""Import, review and correction capture for project shared memory.

Three actions that sit beside the append-only log without changing it:

  import   list candidate lines from Markdown memory someone else wrote (a file,
           a directory, or a repository's .serena/memories) and append the
           selected ones as kind=imported.
  review   list the notes that reached shared memory without a person reading
           them (task-close promotions, journal distillation, imports) and the
           captured corrections still waiting; accept or turn each one down.
  capture  (hook-internal) keep one bounded sentence of a prompt that reads
           like a correction, only when OMS_LESSON_CAPTURE=1.

Every write goes through the memory library: the same sensitive-content scan,
lock and metadata as `agent-memory append`. A review decision is one
append-only receipt row; nothing is edited and no model is called. Being
listed, imported or accepted is a record of what someone decided to keep, not
evidence that the note is true.
"""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile

LIB = pathlib.Path(__file__).resolve().parent
SCHEMA = 1
AUTO_KINDS = ("task_close", "distill", "imported")
MAX_CANDIDATES = 200
MAX_FILES = 50
MAX_FILE_BYTES = 262144
MIN_LEN, MAX_LEN = 12, 400
PENDING_MAX_OPEN = 200
CAPTURE_MAX = 240
ID_RE = re.compile(r"^[A-Za-z0-9._:-]{1,128}$")

# A prompt that reads like the person correcting the agent. Deliberately short
# and anchored at the start of a sentence: a miss costs nothing, a false hit
# costs a line in a review list.
CORRECTION_RE = re.compile(
    r"^(?:no[,.! ]|nope\b|actually[, ]|don'?t\b|do not\b|never\b|always\b|stop\b|"
    r"instead[, ]|please (?:don'?t|never|always)\b|use\b.{1,80}\binstead\b|"
    r"not\b.{1,60},\s*(?:use|but)\b|"
    r"아니[,. ]|아니야|아니요|그게 아니|말고\b|.{1,60}말고 |.{1,60}대신 |"
    r".{0,80}(?:하지 ?마|쓰지 ?마|쓰지 말|하지 말)|항상 |절대 )",
    re.IGNORECASE,
)
SENTENCE_RE = re.compile(r"(?<=[.!?。])\s+|\n+")


def die(message, code=2):
    sys.stderr.write("error: %s\n" % message)
    raise SystemExit(code)


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def normalize(text):
    return " ".join(text.split()).casefold()


def short_hash(text):
    return hashlib.sha256(normalize(text).encode("utf-8")).hexdigest()[:12]


def repo_root(path):
    try:
        done = subprocess.run(["git", "-C", path, "rev-parse", "--show-toplevel"], capture_output=True,
                              text=True, check=False, timeout=10, stdin=subprocess.DEVNULL)
        if done.returncode == 0 and done.stdout.strip():
            return os.path.realpath(done.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    return os.path.realpath(path)


def library(script, *arguments, stdin=None):
    """Run SCRIPT with the memory library sourced; arguments are positional."""
    return subprocess.run(
        ["bash", "-c", '. "$0"; ' + script, str(LIB / "agent-memory-common.sh"), *arguments],
        capture_output=True, text=True, check=False, input=stdin,
        stdin=None if stdin is not None else subprocess.DEVNULL)


def scan(texts):
    """True per text that the sensitive scan passes; a scan that cannot run passes nothing."""
    if not texts:
        return []
    with tempfile.TemporaryDirectory() as scratch:
        paths = []
        for index, text in enumerate(texts):
            path = os.path.join(scratch, "c%d" % index)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write(text + "\n")
            paths.append(path)
        done = library('for f in "$@"; do rc=0; agent_memory_file_sensitive_status "$f" || rc=$?; '
                       'printf "%s\\n" "$rc"; done', *paths)
    codes = done.stdout.split()
    if done.returncode != 0 or len(codes) != len(texts):
        return [False] * len(texts)
    return [code == "1" for code in codes]


def load_index():
    spec = importlib.util.spec_from_file_location("oms_agent_memory_db", str(LIB / "agent-memory-db.py"))
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module  # dataclasses resolve the module by name
    spec.loader.exec_module(module)
    return module


class Store:
    def __init__(self, repo):
        self.repo = repo_root(repo)
        self.dir = os.path.join(self.repo, ".oms", "memory")
        self.shared = os.path.join(self.dir, "shared.md")
        self.pins = os.path.join(self.dir, "pins.md")
        self.receipts = os.path.join(self.dir, "reviews.jsonl")
        self.pending = os.path.join(self.dir, "pending.jsonl")

    def notes(self):
        """(active shared notes, status map); an unreadable log is an empty one."""
        if not os.path.isfile(self.shared):
            return [], {}
        index = load_index()
        entries, _pinned, status = index.memory_state(self.shared, self.pins)
        return [e for e in entries if e.kind != "retirement" and e.event_id not in status], status

    def rows(self, path):
        found = []
        try:
            with open(path, encoding="utf-8") as handle:
                for line in handle:
                    try:
                        row = json.loads(line)
                    except ValueError:
                        continue
                    if isinstance(row, dict) and row.get("schema") == SCHEMA and isinstance(row.get("id"), str):
                        found.append(row)
        except OSError:
            pass
        return found

    def decided(self):
        return {row["id"]: row for row in self.rows(self.receipts) if row.get("decision") in ("accepted", "rejected")}

    def append_row(self, path, row):
        os.makedirs(self.dir, exist_ok=True)
        data = (json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n").encode("utf-8")
        fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        try:
            if os.write(fd, data) != len(data):
                raise OSError("incomplete append")
        finally:
            os.close(fd)
        library('agent_memory_ensure_oms_ignore_for_path "$1" >/dev/null 2>&1 || true', path)

    def append_note(self, text, agent, kind):
        """Append through the library; returns (ok, message)."""
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", suffix=".note", delete=False) as handle:
            handle.write(text.rstrip("\n") + "\n")
            note = handle.name
        try:
            done = library('agent_memory_append_file "$1" project "$2" "$3" "$4"', self.shared, agent, note, kind)
        finally:
            os.unlink(note)
        return done.returncode == 0, (done.stderr.strip().splitlines() or [""])[-1]


def is_auto(entry):
    return (entry.kind in AUTO_KINDS
            or (entry.agent == "journal" and entry.body.lstrip().startswith("journal-distill:")))


def origin(entry):
    if entry.kind == "task_close":
        return "task-close"
    if entry.kind == "imported":
        return "imported"
    return "distill"


def first_line(text, width=160):
    line = " ".join((text.strip().splitlines() or [""])[0].split())
    return line if len(line) <= width else line[: width - 1] + "…"


# ---------------------------------------------------------------- import

def source_files(store, origin_name):
    if origin_name in ("claude", "codex"):
        die("import --from %s is not supported: where a provider keeps its own memory is not something OMS "
            "verifies; pass the file or directory itself with --from PATH" % origin_name)
    if origin_name == "serena":
        base = os.path.join(store.repo, ".serena", "memories")
        if not os.path.isdir(base):
            die("no .serena/memories directory in %s" % store.repo)
    else:
        base = os.path.realpath(os.path.expanduser(origin_name))
        if not os.path.exists(base):
            die("--from %s: no such file or directory" % origin_name)
    if os.path.realpath(base).startswith(os.path.realpath(store.dir) + os.sep) or os.path.realpath(base) == os.path.realpath(store.dir):
        die("--from points at OMS memory itself; there is nothing to import")
    if os.path.isfile(base):
        return [base], 0
    found = []
    for root, dirs, names in os.walk(base):
        dirs[:] = sorted(d for d in dirs if not d.startswith("."))
        for name in sorted(names):
            if name.lower().endswith((".md", ".markdown", ".txt")):
                path = os.path.join(root, name)
                if not os.path.islink(path):
                    found.append(path)
    return found[:MAX_FILES], max(0, len(found) - MAX_FILES)


def label(store, path):
    """Repository-relative when inside the repository, else the file name alone."""
    real = os.path.realpath(path)
    if real.startswith(store.repo + os.sep):
        return os.path.relpath(real, store.repo)
    return os.path.basename(real)


def candidate_lines(path):
    """(line number, text) for bullet items and plain sentences; headings, code and tables are not notes."""
    try:
        if os.path.getsize(path) > MAX_FILE_BYTES:
            return None
        with open(path, encoding="utf-8") as handle:
            lines = handle.read().splitlines()
    except (OSError, UnicodeError):
        return None
    found, fenced, front = [], False, False
    for number, raw in enumerate(lines, 1):
        line = raw.strip()
        if number == 1 and line == "---":
            front = True
            continue
        if front:
            front = line != "---"
            continue
        if line.startswith(("```", "~~~")):
            fenced = not fenced
            continue
        if fenced or not line or line.startswith(("#", "|", "<!--", ">", "---")):
            continue
        text = re.sub(r"^(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?", "", line).strip()
        if MIN_LEN <= len(text) <= MAX_LEN:
            found.append((number, text))
    return found


def run_import(store, args):
    if os.environ.get("OMS_HARNESS_CHILD") == "1" and args.apply:
        die("a harness child cannot import into parent-owned memory; return the request to the parent agent")
    files, extra_files = source_files(store, args.origin)
    existing = {normalize(entry.body) for entry in store.notes()[0]}
    existing |= {normalize(line) for entry in store.notes()[0] for line in entry.body.splitlines() if line.strip()}
    candidates, unread, seen = [], [], set()
    for path in files:
        rows = candidate_lines(path)
        if rows is None:
            unread.append(label(store, path))
            continue
        for number, text in rows:
            digest = short_hash(text)
            if digest in seen:
                continue
            seen.add(digest)
            candidates.append({"n": 0, "path": label(store, path), "line": number, "sha256": digest, "text": text,
                               "status": "duplicate" if normalize(text) in existing else "new"})
    omitted = max(0, len(candidates) - MAX_CANDIDATES)
    candidates = candidates[:MAX_CANDIDATES]
    for candidate, clean in zip(candidates, scan([c["text"] for c in candidates])):
        if not clean:
            candidate["status"] = "withheld"
            candidate["text"] = ""
    for number, candidate in enumerate(candidates, 1):
        candidate["n"] = number

    chosen = []
    if args.apply:
        if args.all == bool(args.select):
            die("import --apply needs exactly one of --select N[,N...] or --all")
        if args.all:
            chosen = [c for c in candidates if c["status"] == "new"]
        else:
            wanted = []
            for part in args.select.split(","):
                if not part.strip().isdigit() or not 1 <= int(part) <= len(candidates):
                    die("--select takes candidate numbers between 1 and %d" % len(candidates))
                wanted.append(int(part))
            for number in dict.fromkeys(wanted):
                candidate = candidates[number - 1]
                if candidate["status"] != "new":
                    die("candidate %d is %s and cannot be imported" % (number, candidate["status"]))
                chosen.append(candidate)
    elif args.select or args.all:
        die("--select/--all apply only with --apply")

    imported, refused = 0, 0
    for candidate in chosen:
        note = "%s\nimported from: %s#L%d sha256:%s" % (candidate["text"], candidate["path"], candidate["line"], candidate["sha256"])
        ok, _message = store.append_note(note, args.agent, "imported")
        candidate["status"] = "imported" if ok else "refused"
        imported += ok
        refused += not ok

    counts = {}
    for candidate in candidates:
        counts[candidate["status"]] = counts.get(candidate["status"], 0) + 1
    if args.json:
        print(json.dumps({"schema": SCHEMA, "kind": "oms-memory-import", "applied": bool(args.apply), "files": len(files),
                          "unread": unread, "files_omitted": extra_files, "candidates_omitted": omitted,
                          "counts": counts, "candidates": candidates}, ensure_ascii=False))
    else:
        print("memory import from %s: %d candidate(s) in %d file(s)%s" % (
            args.origin, len(candidates), len(files), "" if args.apply else "; dry run, nothing written"))
        for candidate in candidates:
            print("%3d  %-9s %s:%d  sha256:%s" % (candidate["n"], candidate["status"], candidate["path"],
                                                 candidate["line"], candidate["sha256"]))
            if candidate["text"]:
                print("       %s" % first_line(candidate["text"]))
        for name in unread:
            print("unread: %s (not UTF-8, or larger than %d bytes)" % (name, MAX_FILE_BYTES))
        if extra_files:
            print("incomplete: %d more file(s) not read (limit %d)" % (extra_files, MAX_FILES))
        if omitted:
            print("incomplete: %d more candidate(s) not listed (limit %d)" % (omitted, MAX_CANDIDATES))
        if args.apply:
            print("memory: imported %d note(s) as kind=imported%s; they wait in `oms agent-memory review`" % (
                imported, ", %d refused by the memory writer" % refused if refused else ""))
        elif counts.get("new"):
            print("next: oms agent-memory import --from %s --apply --select N[,N...]   (or --all)" % args.origin)
        print("note: the source files were only read; an imported line is someone's note, not a verified fact")
    return 1 if refused else 0


# ---------------------------------------------------------------- review

def pending_items(store):
    """Auto-origin notes and captured corrections with no decision yet, oldest first."""
    decided = store.decided()
    items = []
    for entry in store.notes()[0]:
        if is_auto(entry) and entry.event_id not in decided:
            items.append({"id": entry.event_id, "origin": origin(entry), "recorded": entry.occurred_at,
                          "agent": entry.agent, "text": first_line(entry.body)})
    seen = set()
    for row in store.rows(store.pending):
        if row["id"] in decided or row["id"] in seen or not isinstance(row.get("text"), str):
            continue
        seen.add(row["id"])
        items.append({"id": row["id"], "origin": "correction", "recorded": str(row.get("ts") or ""),
                      "agent": "", "text": first_line(row["text"]), "_full": row["text"]})
    return items


def run_review(store, args):
    actions = [name for name in ("accept", "retire", "reject") if getattr(args, name)]
    if len(actions) > 1:
        die("review takes one of --accept, --retire or --reject")
    items = pending_items(store)
    if args.origin_filter:
        allowed = set(args.origin_filter)
        unknown = allowed - {"task-close", "distill", "imported", "correction"}
        if unknown:
            die("--kind takes task-close, distill, imported or correction")
        items = [item for item in items if item["origin"] in allowed]
    if not actions:
        if args.reason:
            die("--reason applies only with --retire or --reject")
        if args.json:
            print(json.dumps({"schema": SCHEMA, "kind": "oms-memory-review", "pending": [
                {k: v for k, v in item.items() if not k.startswith("_")} for item in items]}, ensure_ascii=False))
            return 0
        if not items:
            print("memory review: nothing waiting")
            return 0
        print("memory review: %d waiting (unreviewed; listed, not verified)" % len(items))
        for item in items:
            print("%-10s %s  %s" % (item["origin"], item["recorded"], item["id"]))
            print("    %s" % item["text"])
        print("accept: oms agent-memory review --accept ID    turn down: --retire ID --reason TEXT (a note)"
              " / --reject ID (a correction)")
        return 0

    if os.environ.get("OMS_HARNESS_CHILD") == "1":
        die("a harness child cannot decide a memory review; return the request to the parent agent")
    action = actions[0]
    ident = getattr(args, action)
    if not ID_RE.fullmatch(ident):
        die("invalid memory ID")
    item = next((i for i in pending_items(store) if i["id"] == ident), None)
    if item is None:
        die("%s is not waiting for review (unknown, already decided, superseded or retired)" % ident)
    correction = item["origin"] == "correction"
    if action == "retire" and correction:
        die("%s is a captured correction, not a note; use --reject" % ident)
    if action == "reject" and not correction:
        die("%s is a recorded note; turn it down with --retire ID --reason TEXT" % ident)
    if action == "retire" and not args.reason:
        die("--retire requires --reason")
    if action == "accept" and args.reason:
        die("--reason applies only with --retire or --reject")

    if action == "retire":
        done = subprocess.run(["bash", str(LIB.parent / "agent-memory.sh"), "--repo", store.repo, "--agent", args.agent,
                               "retire", "--id", ident, "--reason", args.reason], check=False,
                              stdin=subprocess.DEVNULL)
        return done.returncode
    receipt = {"schema": SCHEMA, "ts": now(), "id": ident, "agent": args.agent, "origin": item["origin"],
               "decision": "accepted" if action == "accept" else "rejected"}
    if action == "accept" and correction:
        ok, message = store.append_note(item["_full"], args.agent, "lesson")
        if not ok:
            die("the memory writer refused the correction (%s); nothing recorded" % (message or "no reason given"), 3)
    if action == "reject" and args.reason:
        if not scan([args.reason])[0]:
            die("--reason contains sensitive-looking content; nothing recorded", 3)
        receipt["reason"] = " ".join(args.reason.split())[:200]
    store.append_row(store.receipts, receipt)
    if action == "accept":
        print("memory: accepted %s%s" % (ident, " and recorded it as a note" if correction else ""))
    else:
        print("memory: rejected %s; it will not be offered again" % ident)
    return 0


# ---------------------------------------------------------------- capture

def correction_sentence(prompt):
    for part in SENTENCE_RE.split(prompt.strip()):
        sentence = " ".join(part.split())
        if MIN_LEN <= len(sentence) <= CAPTURE_MAX and CORRECTION_RE.match(sentence):
            return sentence
    return ""


def run_capture(store, args):
    """Hook-internal and fail-open: any problem keeps nothing and says nothing."""
    if os.environ.get("OMS_LESSON_CAPTURE") != "1" or os.environ.get("OMS_HARNESS_CHILD") == "1":
        return 0
    if not os.path.isdir(os.path.join(store.repo, ".oms")):
        return 0
    try:
        payload = json.loads(sys.stdin.read() or "{}")
    except ValueError:
        return 0
    prompt = payload.get("prompt") if isinstance(payload, dict) else None
    if not isinstance(prompt, str) or len(prompt) > 4000 or prompt.lstrip().startswith(("<", "[", "{")):
        return 0  # a pasted document or a machine-written prompt is not the person correcting anyone
    sentence = correction_sentence(prompt)
    if not sentence:
        return 0
    ident = "pend-" + short_hash(sentence)
    rows = store.rows(store.pending)
    decided = store.decided()
    if any(row["id"] == ident for row in rows) or ident in decided:
        return 0
    if len({row["id"] for row in rows} - set(decided)) >= PENDING_MAX_OPEN:
        return 0
    if not scan([sentence])[0]:
        return 0
    store.append_row(store.pending, {"schema": SCHEMA, "ts": now(), "id": ident, "text": sentence})
    return 0


def main():
    parser = argparse.ArgumentParser(prog="agent-memory", add_help=False)
    parser.add_argument("command", choices=("import", "review", "capture"))
    parser.add_argument("--repo", default=os.environ.get("OMS_STATE_REPO") or os.getcwd())
    parser.add_argument("--agent", default="")
    parser.add_argument("--from", dest="origin", default="")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--select", default="")
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--kind", dest="origin_filter", action="append", default=[])
    parser.add_argument("--accept", default="")
    parser.add_argument("--retire", default="")
    parser.add_argument("--reject", default="")
    parser.add_argument("--reason", default="")
    parser.add_argument("--json", action="store_true")
    args, extra = parser.parse_known_args()
    if extra:
        die("%s does not take %s (project memory only; --global and --file are not supported)" % (args.command, extra[0]))
    if not os.path.isdir(args.repo):
        die("--repo %s is not a directory" % args.repo)
    store = Store(args.repo)
    if not args.agent:
        args.agent = (library("oms_detect_agent").stdout.strip() or "agent")
    if args.command == "capture":
        try:
            return run_capture(store, args)
        except Exception:  # the hook never fails a prompt
            return 0
    if args.command == "import":
        if not args.origin:
            die("import requires --from serena|PATH")
        if args.apply and args.dry_run:
            die("--apply and --dry-run are opposites")
        if args.accept or args.retire or args.reject or args.reason or args.origin_filter:
            die("--accept/--retire/--reject/--reason/--kind apply only to review")
        return run_import(store, args)
    if args.origin or args.apply or args.dry_run or args.select or args.all:
        die("--from/--apply/--dry-run/--select/--all apply only to import")
    return run_review(store, args)


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError) as error:
        sys.stderr.write("error: memory review: %s\n" % error)
        raise SystemExit(2)
