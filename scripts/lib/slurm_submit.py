#!/usr/bin/env python3
"""Submission-key ledger behind `oms slurm-submit`.

Duplicate detection inside ONE repository's .oms/slurm-submits.jsonl. It is
not exactly-once across machines or clones. The caller (slurm-submit.sh) holds
the repo's file lock around `begin` and `finish`; this module never locks.
Rows are append-only: intent, then submitted or failed (same attempt id).
"""

import argparse
import hashlib
import json
import os
import sys
import time
import uuid

EXIT_REFUSED = 3
KEY_VERSION = 1


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def read_args(path):
    with open(path, "rb") as handle:
        raw = handle.read()
    return [part.decode("utf-8", "surrogateescape") for part in raw.split(b"\0") if part != b""]


def rel_to_root(path, root):
    real = os.path.realpath(path)
    base = os.path.realpath(root)
    if real == base:
        return "."
    if real.startswith(base + os.sep):
        return real[len(base) + 1:]
    return "<outside-repo>/" + os.path.basename(real)


def derive(options):
    """Key and digests. Hashed: script content digest, sbatch args in order
    (as the caller gave them), repo-relative cwd, current commit."""
    args = read_args(options.args_file)
    script_digest = ""
    script_rel = ""
    if options.script:
        with open(options.script, "rb") as handle:
            script_digest = sha256_bytes(handle.read())
        script_rel = rel_to_root(options.script, options.root)
    cwd_rel = rel_to_root(options.cwd, options.root)
    doc = json.dumps({"v": KEY_VERSION, "script_sha256": script_digest, "args": args,
                      "cwd": cwd_rel, "commit": options.commit},
                     sort_keys=True, separators=(",", ":"), ensure_ascii=True)
    default_key = sha256_bytes(doc.encode("ascii"))
    key = options.custom_key or default_key
    short = key[:12] if not options.custom_key else sha256_bytes(key.encode("utf-8", "surrogateescape"))[:12]
    args_digest = sha256_bytes(json.dumps(args, ensure_ascii=True).encode("ascii"))
    return {"key": key, "short": short, "key_source": "custom" if options.custom_key else "default",
            "script": script_rel, "script_sha256": script_digest, "args_sha256": args_digest,
            "cwd": cwd_rel, "commit": options.commit}


def load_rows(path):
    rows = []
    try:
        handle = open(path, encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return rows
    with handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict):
                rows.append(row)
    return rows


def key_state(rows, key):
    """('none'|'submitted'|'unknown'|'failed', info row or None).

    Window: from the last forced intent that did not itself fail, else all
    rows of the key. In the window any submitted attempt blocks; an intent
    with no result blocks as unknown; only failed attempts leave the key free.
    """
    mine = [r for r in rows if r.get("key") == key]
    results = {}
    for row in mine:
        if row.get("type") in ("submitted", "failed"):
            results[row.get("attempt")] = row
    start = 0
    for index, row in enumerate(mine):
        if row.get("type") == "intent" and row.get("forced"):
            outcome = results.get(row.get("attempt"))
            if outcome is None or outcome.get("type") != "failed":
                start = index
    window = mine[start:]
    submitted = [r for r in window if r.get("type") == "submitted"]
    if submitted:
        return "submitted", submitted[-1]
    pending = [r for r in window if r.get("type") == "intent" and r.get("attempt") not in results]
    if pending:
        return "unknown", pending[-1]
    if any(r.get("type") == "failed" for r in window):
        return "failed", None
    return "none", None


def intent_for(rows, attempt):
    for row in rows:
        if row.get("type") == "intent" and row.get("attempt") == attempt:
            return row
    return {}


def describe(state, info, rows, short):
    if state == "submitted":
        job = info.get("job_id") or "(id not parsed from sbatch output)"
        text = ("duplicate: this key was already submitted as job %s at %s (attempt %s)\n"
                "check it: oms run-reconcile list ; squeue -j %s ; sacct -j %s\n"
                % (job, info.get("ts"), info.get("attempt"), info.get("job_id") or "ID", info.get("job_id") or "ID"))
    else:
        intent = intent_for(rows, info.get("attempt"))
        tagged = intent.get("tag")
        where = ("by comment: squeue -u \"$USER\" -o '%%i %%j %%k' | grep 'oms-key=%s' ; "
                 "sacct -u \"$USER\" -o JobID,JobName,Comment | grep 'oms-key=%s'" % (short, short)
                 if tagged else
                 "the comment could not be tagged for that attempt; match by job name or script with "
                 "squeue -u \"$USER\" ; sacct -u \"$USER\" -o JobID,JobName,Comment")
        text = ("unknown: an earlier submission with this key may have gone through "
                "(intent recorded %s, no result row; OMS may have died before sbatch answered)\n"
                "look for it %s\n" % (info.get("ts"), where))
    return text + ("not submitting. This is duplicate detection inside this repository's .oms, "
                   "not exactly-once across machines or clones.\n"
                   "to submit anyway: --force-resubmit --reason TEXT\n")


def now():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def append(path, row):
    os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
    line = json.dumps(row, sort_keys=True, ensure_ascii=True) + "\n"
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        os.write(fd, line.encode("ascii"))
        os.fsync(fd)
    finally:
        os.close(fd)


def command_key(options):
    print(json.dumps(derive(options), sort_keys=True))
    return 0


def command_status(options):
    info = derive(options)
    rows = load_rows(options.file)
    state, row = key_state(rows, info["key"])
    result = {"state": state}
    if row:
        result.update(job_id=row.get("job_id", ""), ts=row.get("ts", ""), attempt=row.get("attempt", ""))
    print(json.dumps(result, sort_keys=True))
    return 0


def command_begin(options):
    info = derive(options)
    rows = load_rows(options.file)
    state, row = key_state(rows, info["key"])
    if state in ("submitted", "unknown") and not options.force:
        sys.stderr.write(describe(state, row, rows, info["short"]))
        return EXIT_REFUSED
    attempt = uuid.uuid4().hex[:12]
    record = {"schema": 1, "type": "intent", "key": info["key"], "attempt": attempt, "ts": now(),
              "key_source": info["key_source"], "script": info["script"],
              "script_sha256": info["script_sha256"], "args_sha256": info["args_sha256"],
              "cwd": info["cwd"], "commit": info["commit"], "tag": options.tag == "yes"}
    if options.store_args and options.args_file:
        record["args"] = read_args(options.args_file)
    if options.force:
        record["forced"] = True
        record["reason"] = options.reason
        record["overrides"] = state
    append(options.file, record)
    print(json.dumps({"attempt": attempt, "short": info["short"], "key": info["key"]}, sort_keys=True))
    return 0


def command_finish(options):
    row = {"schema": 1, "key": options.key, "attempt": options.attempt, "ts": now()}
    if options.exit_code is not None:
        row.update(type="failed", exit=options.exit_code)
    else:
        row.update(type="submitted", job_id=options.job_id or "", cluster_name=options.cluster_name or "")
    append(options.file, row)
    return 0


def build_parser():
    parser = argparse.ArgumentParser(prog="slurm_submit.py")
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("key", "status", "begin"):
        p = sub.add_parser(name)
        p.add_argument("--root", required=True)
        p.add_argument("--cwd", required=True)
        p.add_argument("--commit", required=True)
        p.add_argument("--script", default="")
        p.add_argument("--custom-key", default="")
        p.add_argument("--args-file", required=True)
        if name != "key":
            p.add_argument("--file", required=True)
        if name == "begin":
            p.add_argument("--force", action="store_true")
            p.add_argument("--reason", default="")
            p.add_argument("--tag", default="no")
            p.add_argument("--store-args", action="store_true")
    fin = sub.add_parser("finish")
    fin.add_argument("--file", required=True)
    fin.add_argument("--key", required=True)
    fin.add_argument("--attempt", required=True)
    fin.add_argument("--job-id", default="")
    fin.add_argument("--cluster-name", default="")
    fin.add_argument("--exit-code", type=int, default=None)
    return parser


def main(argv):
    options = build_parser().parse_args(argv)
    handler = {"key": command_key, "status": command_status,
               "begin": command_begin, "finish": command_finish}[options.command]
    return handler(options)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
