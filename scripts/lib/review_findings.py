#!/usr/bin/env python3
"""Review findings: where each one points, and what became of it.

A gate seat may list its findings in a fenced ``findings`` block, one JSON
object per line. `collect` reads those blocks from the seats' final answers,
checks each citation against the diff the seats were shown and the file it
names, and writes `_findings-RUN.jsonl` beside the run's review-scope
artifact. No model is called and the gate verdict is never touched: the
classification says whether a citation can be found, not whether the finding
is right.

The lifecycle (`fix`, `verify`, `dismiss`, `out-of-scope`, `reopen`) appends to
`_finding-events.jsonl` in the same directory. Every status is a record of
who said what, never approval or verification evidence by itself: `verify`
only links a finding to a mechanical verify receipt the artifact index
already holds, with matching review scope and fix commit, and refuses
without that provenance.
"""

import argparse
import hashlib
import json
import os
import re
import runpy
import subprocess
import sys
import time

SCHEMA = 1
MAX_FINDINGS_PER_SEAT = 50
MAX_TEXT = 500
QUOTE_WINDOW = 3
SEVERITIES = ("blocking", "advisory")
ANCHORS = ("in-diff", "context", "outside-scope", "anchor-not-found", "unclassified")
STATES = ("open", "fixed", "verified", "dismissed", "out-of-scope")
EVENTS_FILE = "_finding-events.jsonl"
BLOCK_OPEN = re.compile(r"^\s*```+\s*findings\s*$")
BLOCK_CLOSE = re.compile(r"^\s*```+\s*$")
HUNK = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")
ID_RE = re.compile(r"^f-[0-9a-f]{12}$")
RUN_RE = re.compile(r"^[A-Za-z0-9TZ-]{1,64}$")
COMMIT_RE = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")


def clean(value, limit=MAX_TEXT):
    """One printable line: provider text never reaches a terminal or a row with controls in it."""
    text = "".join(" " if ch in "\t\r\n" else ch for ch in str(value)
                   if ch in "\t\r\n" or (ord(ch) >= 32 and not 127 <= ord(ch) <= 159))
    return " ".join(text.split())[:limit]


def normalized(value):
    return " ".join(str(value).lower().split())


def finding_id(path, summary, quote):
    body = "\0".join((path, normalized(summary), normalized(quote or "")))
    return "f-" + hashlib.sha256(body.encode("utf-8", "surrogateescape")).hexdigest()[:12]


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def read_text(path, limit=8 * 1024 * 1024):
    try:
        with open(path, "rb") as handle:
            return handle.read(limit + 1)[:limit].decode("utf-8", "replace")
    except OSError:
        return None


def parse_blocks(text):
    """(findings, rejected) from every fenced findings block of one seat's answer."""
    found, rejected, inside = [], 0, False
    for line in text.splitlines():
        if not inside:
            inside = bool(BLOCK_OPEN.match(line))
            continue
        if BLOCK_CLOSE.match(line):
            inside = False
            continue
        if not line.strip():
            continue
        row = None
        try:
            row = json.loads(line)
        except ValueError:
            pass
        item = valid_finding(row)
        if item is None or len(found) >= MAX_FINDINGS_PER_SEAT:
            rejected += 1
        else:
            found.append(item)
    return found, rejected


def valid_finding(row):
    if not isinstance(row, dict):
        return None
    path, line, summary = row.get("file"), row.get("line"), row.get("summary")
    severity, quote = row.get("severity", "blocking"), row.get("quote")
    if not isinstance(path, str) or not path or "\0" in path or len(path) > 1024:
        return None
    path = path[2:] if path.startswith("./") else path
    if os.path.isabs(path) or ".." in path.split("/") or clean(path, 1024) != path:
        return None
    if type(line) is not int or line < 1:
        return None
    if not isinstance(summary, str) or not clean(summary):
        return None
    if severity not in SEVERITIES:
        return None
    if quote is not None and not isinstance(quote, str):
        return None
    quote = clean(quote) if quote else ""
    return {"file": path, "line": line, "severity": severity, "summary": clean(summary), "quote": quote}


def diff_lines(text):
    """{path: (added new-side lines, context new-side lines)} of a unified git diff."""
    files, current, line = {}, None, 0
    for raw in text.splitlines():
        if raw.startswith("diff --git "):
            current = None
        elif raw.startswith("+++ "):
            name = raw[4:].split("\t", 1)[0]
            current = None
            if name.startswith("b/"):
                current = files.setdefault(name[2:], (set(), set()))
        elif raw.startswith("--- "):
            continue
        elif raw.startswith("@@"):
            match = HUNK.match(raw)
            line = int(match.group(1)) if match else 0
            if not match:
                current = None
        elif current is not None and line:
            if raw.startswith("+"):
                current[0].add(line)
                line += 1
            elif raw.startswith(" "):
                current[1].add(line)
                line += 1
            elif raw.startswith("-") or raw.startswith("\\"):
                continue
            else:
                current = None
    return files


def source_lines(repo, path):
    """The cited file's lines, None when it is absent or resolves outside the repository."""
    root = os.path.realpath(repo)
    full = os.path.realpath(os.path.join(root, path))
    if full != root and not full.startswith(root + os.sep):
        return None
    if not os.path.isfile(full):
        return None
    text = read_text(full)
    return None if text is None else text.splitlines()


def classify(finding, repo, diff):
    """(anchor, quote_found). The anchor says where the citation lands, never whether the finding is right."""
    lines = source_lines(repo, finding["file"])
    if lines is None or finding["line"] > len(lines):
        return "anchor-not-found", (False if finding["quote"] else None)
    quote_found = None
    if finding["quote"]:
        wanted = normalized(finding["quote"])
        low, high = max(0, finding["line"] - 1 - QUOTE_WINDOW), finding["line"] + QUOTE_WINDOW
        quote_found = any(wanted and wanted in normalized(text) for text in lines[low:high])
        if not quote_found:
            return "anchor-not-found", False
    if diff is None:
        return "unclassified", quote_found
    added, context = diff.get(finding["file"], (set(), set()))
    if finding["line"] in added:
        return "in-diff", quote_found
    if finding["line"] in context:
        return "context", quote_found
    return "outside-scope", quote_found


def append_rows(path, rows):
    data = "".join(json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n" for row in rows)
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        os.write(fd, data.encode("utf-8", "replace"))
    finally:
        os.close(fd)


def read_rows(path):
    rows = []
    text = read_text(path)
    for line in (text or "").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict) and row.get("schema") == SCHEMA:
            rows.append(row)
    return rows


def collect(args):
    if not RUN_RE.match(args.run):
        raise SystemExit("review findings: unsafe run id")
    diff = None
    if args.diff and os.path.isfile(args.diff) and os.path.getsize(args.diff) > 0:
        text = read_text(args.diff, 64 * 1024 * 1024)
        diff = diff_lines(text) if text is not None else None
    scope_name, diff_sha = "", ""
    if args.scope:
        scope_name = os.path.basename(args.scope)
        try:
            with open(args.scope, encoding="utf-8") as handle:
                diff_sha = str(json.load(handle).get("diff_sha256") or "")
        except (OSError, ValueError, AttributeError):
            diff_sha = ""
    rows, rejected, stamp = [], 0, now()
    seats = sorted(name for name in os.listdir(args.windows) if name.endswith(".txt")) if os.path.isdir(args.windows) else []
    for name in seats:
        text = read_text(os.path.join(args.windows, name))
        if text is None:
            continue
        found, bad = parse_blocks(text)
        rejected += bad
        seen = set()
        for finding in found:
            ident = finding_id(finding["file"], finding["summary"], finding["quote"])
            if ident in seen:
                continue
            seen.add(ident)
            anchor, quote_found = classify(finding, args.repo, diff)
            rows.append(dict(finding, schema=SCHEMA, kind="review-finding", id=ident, run=args.run,
                             seat=clean(name[:-4], 80), anchor=anchor, quote_found=quote_found,
                             scope=scope_name, diff_sha256=diff_sha, ts=stamp))
    if not rows and not rejected:
        return 0
    target = ""
    if rows:
        target = os.path.join(args.artifact_dir, "_findings-%s.jsonl" % args.run)
        append_rows(target, rows)
    counts = {}
    for row in rows:
        counts[row["anchor"]] = counts.get(row["anchor"], 0) + 1
    parts = ["%d %s" % (counts[name], name) for name in ANCHORS if counts.get(name)]
    line = "findings: %d recorded" % len(rows)
    if parts:
        line += " (" + ", ".join(parts) + ")"
    if rejected:
        line += "; %d rejected as malformed" % rejected
    if target:
        line += " -> " + target
    print(line)
    if counts.get("anchor-not-found"):
        print("findings: an anchor-not-found citation names a file, line or quote that is not there; "
              "it is not usable as blocking evidence (the GATE verdict is unchanged)")
    return 0


def index_rows(repo):
    return read_rows_any(os.path.join(repo, ".oms", "artifacts", "index.jsonl"))


def commit_sha(repo, value):
    """Pin an explicit commit SHA while recording a fix; refs are not stable claims."""
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{4,64}", value):
        raise SystemExit("review findings: --commit must name an existing commit SHA")
    result = subprocess.run(["git", "-C", repo, "rev-parse", "--verify", value.lower() + "^{commit}"],
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False)
    full = result.stdout.decode("ascii", "replace").strip()
    if result.returncode != 0 or not COMMIT_RE.fullmatch(full):
        raise SystemExit("review findings: --commit must name an existing commit SHA in --repo")
    return full


def read_scope(root, path, label):
    if (not isinstance(path, str) or not path or os.path.isabs(path) or
            ".." in path.split("/") or "\0" in path):
        raise SystemExit("review findings: unsafe %s scope path" % label)
    durable = runpy.run_path(os.path.join(os.path.dirname(os.path.abspath(__file__)), "durable-jsonl.py"))
    raw = durable["read_no_follow"](root, path, "review findings: %s scope" % label, max_bytes=8 * 1024 * 1024)
    try:
        scope = json.loads(raw)
    except (ValueError, UnicodeError):
        scope = None
    if (not isinstance(scope, dict) or scope.get("schema") != SCHEMA or scope.get("kind") != "review-scope" or
            scope.get("diff_included") is not True or
            not all(isinstance(scope.get(key), str) and COMMIT_RE.fullmatch(scope[key]) for key in ("base", "head")) or
            not isinstance(scope.get("diff_sha256"), str) or not SHA256_RE.fullmatch(scope["diff_sha256"]) or
            not isinstance(scope.get("paths"), list) or
            not all(isinstance(path, str) and path and not os.path.isabs(path) and ".." not in path.split("/")
                    and "\0" not in path for path in scope["paths"])):
        raise SystemExit("review findings: verification needs a complete review scope with pinned base, HEAD and diff; "
                         "rerun the gate with the original --base")
    return scope, hashlib.sha256(raw).hexdigest()


def verify_binding(args, entry, fixed, receipt):
    source, digest = receipt.get("source"), receipt.get("source_sha256")
    if not source or not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
        raise SystemExit("review findings: that verification receipt has no pinned review scope; "
                         "rerun peer-review --gate --verify with the original --base")
    scope, actual_digest = read_scope(args.repo, source, "verification")
    if actual_digest != digest:
        raise SystemExit("review findings: verification scope digest differs from the indexed receipt; rerun the gate")
    if scope.get("tracked_clean") is not True:
        raise SystemExit("review findings: verification scope must record a clean tracked tree at the fix commit; rerun the gate")
    original, _ = read_scope(args.artifact_dir, entry.get("scope"), "finding")
    if entry.get("diff_sha256") != original["diff_sha256"]:
        raise SystemExit("review findings: finding no longer matches its recorded review scope; rerun the review")
    # A fix changes the diff bytes. Retain the original base and require this
    # citation's path, rather than equating old and new diff digests.
    if scope["base"] != original["base"]:
        raise SystemExit("review findings: verification scope does not match the finding's review base; "
                         "rerun the gate with --base %s" % original["base"])
    if entry.get("file") not in scope["paths"]:
        raise SystemExit("review findings: verification scope does not cover the cited path %s" % clean(entry.get("file"), 200))
    if not fixed.get("commit"):
        raise SystemExit("review findings: fix record has no commit; reopen and mark fixed with --commit SHA, then rerun the gate")
    commit = commit_sha(args.repo, fixed["commit"])
    if scope["head"] != commit:
        raise SystemExit("review findings: verification scope HEAD does not match the recorded fix commit %s" % commit)
    return commit


def read_rows_any(path):
    rows = []
    for line in (read_text(path, 64 * 1024 * 1024) or "").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if isinstance(row, dict):
            rows.append(row)
    return rows


def load(directory):
    """({id: latest sighting with `runs`}, {id: [events]}) from the review artifact directory."""
    findings, events = {}, {}
    names = sorted(name for name in os.listdir(directory)
                   if name.startswith("_findings-") and name.endswith(".jsonl")) if os.path.isdir(directory) else []
    for name in names:
        for row in read_rows(os.path.join(directory, name)):
            ident = row.get("id")
            if row.get("kind") != "review-finding" or not isinstance(ident, str) or not ID_RE.match(ident):
                continue
            entry = findings.get(ident)
            sighting = (str(row.get("ts") or ""), str(row.get("run") or ""))
            if entry is None or sighting >= (entry["ts"], entry["run"]):
                runs = entry["runs"] if entry else []
                entry = dict(row, ts=sighting[0], run=sighting[1], runs=runs)
                findings[ident] = entry
            if sighting not in entry["runs"]:
                entry["runs"].append(sighting)
    for row in read_rows(os.path.join(directory, EVENTS_FILE)):
        if row.get("kind") == "review-finding-event" and isinstance(row.get("id"), str):
            events.setdefault(row["id"], []).append(row)
    return findings, events


def status(entry, history):
    """(state, note). A fixed or verified finding a later review reported again reads open."""
    if not history:
        return "open", ""
    last = history[-1]
    state = last.get("state") if last.get("state") in STATES else "open"
    if state in ("fixed", "verified"):
        known = last.get("runs") if isinstance(last.get("runs"), list) else []
        again = [run for _, run in sorted(entry["runs"]) if run not in known]
        if again:
            return "open", "reported again in run %s after it was marked %s" % (clean(again[-1], 64), state)
    return state, clean(last.get("reason") or last.get("note") or "", 200)


def resolve(directory, ident):
    findings, events = load(directory)
    matches = [key for key in findings if key == ident or (len(ident) >= 6 and key.startswith(ident))]
    if len(matches) != 1:
        raise SystemExit("review findings: %s finding id: %s" % ("ambiguous" if matches else "unknown", clean(ident, 40)))
    return matches[0], findings[matches[0]], events.get(matches[0], [])


def record(directory, ident, state, entry, **fields):
    # The runs that had reported the finding when this was said: a run outside the list is a later sighting.
    row = {"schema": SCHEMA, "kind": "review-finding-event", "id": ident, "state": state, "ts": now(),
           "runs": sorted(run for _, run in entry["runs"])}
    row.update({key: value for key, value in fields.items() if value})
    append_rows(os.path.join(directory, EVENTS_FILE), [row])
    return row


def confirmations(directory):
    here = os.path.dirname(os.path.abspath(__file__))  # isolated mode leaves the script's directory off the path
    if here not in sys.path:
        sys.path.insert(0, here)
    import review_confirm
    return review_confirm.latest(directory), review_confirm.note


def listing(args):
    findings, events = load(args.artifact_dir)
    confirmed, confirm_note = confirmations(args.artifact_dir)
    rows = []
    for ident in sorted(findings, key=lambda key: (findings[key]["ts"], key)):
        entry = findings[ident]
        state, note = status(entry, events.get(ident, []))
        if args.status and state != args.status:
            continue
        rows.append({"id": ident, "status": state, "note": note, "severity": entry.get("severity"),
                     "anchor": entry.get("anchor"), "quote_found": entry.get("quote_found"),
                     "file": entry.get("file"), "line": entry.get("line"), "summary": entry.get("summary"),
                     "seat": entry.get("seat"), "run": entry.get("run"), "sightings": len(entry["runs"])})
        if ident in confirmed:
            rows[-1]["confirmation"] = {key: confirmed[ident].get(key) for key in ("label", "by", "run", "reason")}
            rows[-1]["note"] = "; ".join(part for part in (note, confirm_note(confirmed[ident])) if part)
    if args.json:
        print(json.dumps({"schema": SCHEMA, "findings": rows}, ensure_ascii=False, sort_keys=True))
        return 0
    if not rows:
        print("review findings: none" + (" with status " + args.status if args.status else " recorded"))
        return 0
    for row in rows:
        print("%s  %-9s %-8s %-16s %s:%s  %s%s" % (
            row["id"], row["status"], row["severity"], row["anchor"], clean(row["file"], 200), row["line"],
            clean(row["summary"], 160), "  [%s]" % row["note"] if row["note"] else ""))
    print("status is a record of what was said about a finding, not approval or verification evidence")
    return 0


def undisposed(args):
    """Findings still open that a run before RUN reported (a repeat counts): one line each, nothing when all have a disposition."""
    findings, events = load(args.artifact_dir)
    for ident in sorted(findings, key=lambda key: (findings[key]["ts"], key)):
        entry = findings[ident]
        if all(run == args.run for _, run in entry["runs"]) or status(entry, events.get(ident, []))[0] != "open":
            continue
        print("undisposed %s  %s  %s:%s  %s" % (ident, entry.get("severity"), clean(entry.get("file"), 200),
                                              entry.get("line"), clean(entry.get("summary"), 160)))
    return 0


def show(args):
    ident, entry, history = resolve(args.artifact_dir, args.id)
    state, note = status(entry, history)
    print("%s  %s%s" % (ident, state, "  [%s]" % note if note else ""))
    print("  %s:%s  %s  anchor=%s  quote_found=%s" % (
        clean(entry.get("file"), 200), entry.get("line"), entry.get("severity"), entry.get("anchor"),
        {True: "yes", False: "no"}.get(entry.get("quote_found"), "-")))
    print("  summary: " + clean(entry.get("summary")))
    if entry.get("quote"):
        print("  quote: " + clean(entry["quote"]))
    print("  reported by %s in %d run(s), last %s" % (clean(entry.get("seat"), 80), len(entry["runs"]), clean(entry.get("run"), 64)))
    confirmed, confirm_note = confirmations(args.artifact_dir)
    if ident in confirmed:
        print("  second opinion (run %s): %s%s" % (
            clean(confirmed[ident].get("run"), 64), confirm_note(confirmed[ident]),
            ": " + clean(confirmed[ident].get("reason"), 300) if confirmed[ident].get("reason") else ""))
    for event in history:
        detail = clean(event.get("reason") or event.get("note") or "", 200)
        extra = " verify=%s" % clean(event["verify_artifact_id"], 80) if event.get("verify_artifact_id") else ""
        extra += " commit=%s" % clean(event["commit"], 64) if event.get("commit") else ""
        print("  %s -> %s%s%s" % (clean(event.get("ts"), 32), clean(event.get("state"), 16), extra,
                                  ": " + detail if detail else ""))
    return 0


def transition(args):
    ident, entry, history = resolve(args.artifact_dir, args.id)
    state, _ = status(entry, history)
    if args.action == "fix":
        if state != "open":
            raise SystemExit("review findings: %s is %s; only an open finding can be marked fixed" % (ident, state))
        commit = commit_sha(args.repo, args.commit) if args.commit else ""
        record(args.artifact_dir, ident, "fixed", entry, note=clean(args.note or ""), commit=commit)
        print("%s: fixed (a claim; `findings verify --run REF` links the mechanical receipt)" % ident)
    elif args.action == "verify":
        if state != "fixed":
            raise SystemExit("review findings: %s is %s; mark it fixed before verifying" % (ident, state))
        fixed_at = str(history[-1].get("ts") or "")
        receipt = None
        for row in index_rows(args.repo):
            if row.get("kind") != "review-verify" or type(row.get("exit")) is not int:
                continue
            names = (row.get("artifact_id"), row.get("artifact"), os.path.basename(str(row.get("artifact") or "")))
            if args.run in names:
                receipt = row
        if receipt is None:
            raise SystemExit("review findings: no review-verify receipt named %s in the artifact index; "
                             "verified needs a recorded verification run, not a claim" % clean(args.run, 120))
        if receipt["exit"] != 0:
            raise SystemExit("review findings: that verification run failed (exit %d)" % receipt["exit"])
        if str(receipt.get("ts") or "") < fixed_at:
            raise SystemExit("review findings: that verification run predates the fix record (%s)" % fixed_at)
        commit = verify_binding(args, entry, history[-1], receipt)
        record(args.artifact_dir, ident, "verified", entry, verify_artifact_id=clean(receipt.get("artifact_id") or "", 120),
               verify_artifact=clean(receipt.get("artifact") or "", 400), commit=commit, note=clean(args.note or ""))
        print("%s: verified against %s" % (ident, clean(receipt.get("artifact_id") or args.run, 120)))
    elif args.action in ("dismiss", "out-of-scope"):
        state_word = "dismissed" if args.action == "dismiss" else "out-of-scope"
        if os.environ.get("OMS_HARNESS_CHILD") == "1":
            raise SystemExit("review findings: a harness worker or reviewer cannot %s a finding; the owner does" % args.action)
        if state not in ("open", "fixed"):
            raise SystemExit("review findings: %s is %s; nothing to %s" % (ident, state, args.action))
        reason = clean(args.reason or "")
        if not reason:
            raise SystemExit("review findings: %s needs --reason TEXT" % args.action)
        record(args.artifact_dir, ident, state_word, entry, reason=reason)
        print("%s: %s (%s)" % (ident, state_word, reason))
    else:
        if state == "open":
            raise SystemExit("review findings: %s is already open" % ident)
        reason = clean(args.reason or "")
        if not reason:
            raise SystemExit("review findings: reopen needs --reason TEXT")
        record(args.artifact_dir, ident, "open", entry, reason=reason)
        print("%s: open (%s)" % (ident, reason))
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="peer-review.sh findings")
    sub = parser.add_subparsers(dest="action", required=True)
    one = sub.add_parser("collect")
    one.add_argument("--run", required=True)
    one.add_argument("--artifact-dir", required=True)
    one.add_argument("--repo", required=True)
    one.add_argument("--diff", default="")
    one.add_argument("--scope", default="")
    one.add_argument("--windows", required=True)
    one = sub.add_parser("list")
    one.add_argument("--status", choices=STATES)
    one.add_argument("--json", action="store_true")
    one = sub.add_parser("undisposed")
    one.add_argument("--run", required=True)
    one = sub.add_parser("show")
    one.add_argument("id")
    for name in ("fix", "verify", "dismiss", "out-of-scope", "reopen"):
        one = sub.add_parser(name)
        one.add_argument("id")
        one.add_argument("--note")
        one.add_argument("--reason")
        one.add_argument("--commit")
        one.add_argument("--run", required=name == "verify")
    for one in sub.choices.values():
        if one.prog.endswith("collect"):
            continue
        one.add_argument("--artifact-dir", default=os.path.join(os.getcwd(), ".oms", "artifacts", "review"))
        one.add_argument("--repo", default=os.getcwd())
    args = parser.parse_args(argv)
    if args.action == "collect":
        return collect(args)
    if args.action == "list":
        return listing(args)
    if args.action == "show":
        return show(args)
    if args.action == "undisposed":
        return undisposed(args)
    return transition(args)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except BrokenPipeError:
        sys.exit(0)
    except SystemExit as stop:
        if isinstance(stop.code, str):  # a refusal: say why and exit like a usage error
            print(stop.code, file=sys.stderr)
            sys.exit(2)
        raise
