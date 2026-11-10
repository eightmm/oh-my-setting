"""Claims and figure records: which recorded run a stated number or a figure came from.

Two append-only JSONL files under .oms. Nothing here runs a training command,
a figure script or a model; `check` and `verify` only read what was recorded
and compare it with what is on disk now.
"""

import hashlib
import json
import math
import os
import re
import subprocess
import sys

SCHEMA = 1
NAME_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")
MAX_TEXT = 300
MAX_RUNS = 32
HASH_MAX = 536870912


def fail(message):
    sys.stderr.write("error: %s\n" % message)
    sys.exit(2)


def one_line(text, limit=MAX_TEXT):
    text = " ".join(str(text).split())
    return text if len(text) <= limit else text[:limit - 1] + "…"


def number(text, label):
    try:
        value = float(text)
    except (TypeError, ValueError):
        fail("%s must be a number: %r" % (label, text))
    if not math.isfinite(value):
        fail("%s must be finite: %r" % (label, text))
    return value


def rows(path):
    """Parsed rows of one JSONL file; unreadable lines are counted, never guessed at."""
    found, bad = [], 0
    try:
        handle = open(path, encoding="utf-8", errors="replace")
    except OSError:
        return found, bad
    with handle:
        for line in handle:
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except ValueError:
                bad += 1
                continue
            if isinstance(row, dict) and isinstance(row.get("id"), str):
                found.append(row)
            else:
                bad += 1
    return found, bad


def latest(path):
    """One row per id, the last one written, in first-seen order: re-recording an id corrects it."""
    found, bad = rows(path)
    by_id = {}
    for row in found:
        by_id[row["id"]] = row
    return list(by_id.values()), bad


def capsule_for(run, runs_dir, spine):
    """(capsule id, capsule dict) for a capsule id or a spine run id, else (None, None)."""
    if not isinstance(run, str) or not run or "/" in run or run in (".", ".."):
        return None, None
    path = os.path.join(runs_dir, run, "capsule.json")
    if not os.path.isfile(path):
        path = None
        try:
            with open(spine, encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    try:
                        row = json.loads(line)
                    except ValueError:
                        continue
                    if (isinstance(row, dict) and row.get("run_id") == run
                            and row.get("tool") == "run-capsule" and row.get("path")):
                        path = row["path"]  # the run's last capsule, as `run diff` reads it
        except OSError:
            path = None
        if not path or not os.path.isfile(path):
            return None, None
    try:
        with open(path, encoding="utf-8") as handle:
            capsule = json.load(handle)
    except (OSError, ValueError):
        return None, None
    if not isinstance(capsule, dict) or not isinstance(capsule.get("id"), str):
        return None, None
    return capsule["id"], capsule


def recorded_metric(capsule, metric):
    metrics = (capsule.get("result") or {}).get("metrics")
    value = metrics.get(metric) if isinstance(metrics, dict) else None
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        return None
    return value


def sha256_file(path):
    """Hex digest of a regular file up to the size cap, else None."""
    try:
        if not os.path.isfile(path) or os.path.getsize(path) > HASH_MAX:
            return None
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
        return digest.hexdigest()
    except OSError:
        return None


def inside(root, path):
    """Repo-relative spelling of a path inside the repository, else None."""
    real_root = os.path.realpath(root)
    real = os.path.realpath(path)
    if real != real_root and not real.startswith(real_root + os.sep):
        return None
    return os.path.relpath(real, real_root)


def new_id(prefix, existing, stamp):
    base = "%s-%s" % (prefix, stamp.replace("-", "").replace(":", ""))
    name, n = base, 1
    while name in existing:
        n += 1
        name = "%s-%d" % (base, n)
    return name


def emit(row):
    print(json.dumps(row, ensure_ascii=False, allow_nan=False, sort_keys=True))


# ---- claims ---------------------------------------------------------------

def claims_add(env, args):
    opts = {"metric": None, "value": None, "run": None, "tolerance": "0", "where": "", "id": None}
    i = 0
    while i < len(args):
        key = args[i]
        if key.startswith("--") and key[2:] in opts:
            if i + 1 >= len(args):
                fail("%s requires a value" % key)
            opts[key[2:]] = args[i + 1]
            i += 2
        else:
            fail("unknown claims add argument: %s" % key)
    for need in ("metric", "value", "run"):
        if not opts[need]:
            fail("claims add requires --%s" % need)
    value = number(opts["value"], "--value")
    tolerance = number(opts["tolerance"], "--tolerance")
    if tolerance < 0:
        fail("--tolerance must not be negative")
    capsule_id, capsule = capsule_for(opts["run"], env["runs"], env["spine"])
    if capsule is None:
        fail("--run %s: no capsule recorded for it (capture one with: oms run capsule run --metrics FILE -- CMD)"
             % opts["run"])
    recorded = recorded_metric(capsule, opts["metric"])
    if recorded is None:
        fail("capsule %s recorded no numeric metric %r" % (capsule_id, opts["metric"]))
    existing = {row["id"] for row in rows(env["claims"])[0]}
    ident = opts["id"] or new_id("claim", existing, env["ts"])
    if not NAME_RE.match(ident):
        fail("--id must match [A-Za-z0-9][A-Za-z0-9._-]{0,63}")
    emit({"schema": SCHEMA, "id": ident, "ts": env["ts"], "agent": env["agent"],
          "where": one_line(opts["where"]), "metric": one_line(opts["metric"], 120),
          "value": value, "tolerance": tolerance, "run": opts["run"], "capsule": capsule_id})
    state = "matches" if abs(value - recorded) <= tolerance else "DOES NOT MATCH"
    sys.stderr.write("claim %s: %s=%s %s the recorded %s (tolerance %s) in capsule %s\n"
                     % (ident, opts["metric"], fmt(value), state, fmt(recorded), fmt(tolerance), capsule_id))


def fmt(value):
    return "%g" % value if isinstance(value, float) else str(value)


def capsule_verifies(env, capsule_id, cache):
    """True / False from `run capsule verify`, None when it could not be asked."""
    if capsule_id not in cache:
        verdict = None
        try:
            done = subprocess.run([env["capsule_tool"], "verify", capsule_id], capture_output=True,
                                  text=True, timeout=120, stdin=subprocess.DEVNULL, check=False)
            verdict = done.returncode == 0 if done.returncode in (0, 1) else None
        except (OSError, subprocess.SubprocessError):
            verdict = None
        cache[capsule_id] = verdict
    return cache[capsule_id]


def claims_check(env, args):
    as_json = strict = False
    verify = True
    only = None
    for arg in args:
        if arg == "--json":
            as_json = True
        elif arg == "--strict":
            strict = True
        elif arg == "--no-verify":
            verify = False
        elif arg.startswith("-"):
            fail("unknown claims check argument: %s" % arg)
        elif only is None:
            only = arg
        else:
            fail("claims check takes at most one claim id")
    claims, bad = latest(env["claims"])
    if only is not None:
        claims = [row for row in claims if row["id"] == only]
        if not claims:
            fail("no such claim: %s" % only)
    results, cache = [], {}
    for claim in claims:
        result = {"id": claim["id"], "where": claim.get("where") or "", "metric": claim.get("metric"),
                  "claimed": claim.get("value"), "tolerance": claim.get("tolerance") or 0,
                  "capsule": claim.get("capsule"), "recorded": None, "notes": []}
        capsule_id, capsule = capsule_for(claim.get("capsule"), env["runs"], env["spine"])
        claimed = claim.get("value")
        if capsule is None:
            result["status"] = "missing"
            result["notes"].append("capsule not found")
        elif isinstance(claimed, bool) or not isinstance(claimed, (int, float)):
            result["status"] = "missing"
            result["notes"].append("claim row carries no number")
        else:
            recorded = recorded_metric(capsule, claim.get("metric"))
            result["recorded"] = recorded
            if recorded is None:
                result["status"] = "missing"
                result["notes"].append("capsule records no numeric %s" % claim.get("metric"))
            else:
                result["status"] = "match" if abs(claimed - recorded) <= result["tolerance"] else "mismatch"
            exit_code = (capsule.get("result") or {}).get("exit")
            if exit_code not in (0, None):
                result["notes"].append("run exited %s" % exit_code)
            if (capsule.get("git") or {}).get("dirty"):
                result["notes"].append("run had uncommitted changes")
            if len(capsule.get("seeds") or []) <= 1:
                result["notes"].append("single seed")
            if verify:
                verdict = capsule_verifies(env, capsule_id, cache)
                result["capsule_verifies"] = verdict
                if verdict is False:
                    result["notes"].append("stale: the tree no longer matches the run's capsule")
                elif verdict is None:
                    result["notes"].append("capsule verify could not run")
        results.append(result)
    counts = {}
    for result in results:
        counts[result["status"]] = counts.get(result["status"], 0) + 1
    stale = sum(1 for result in results if result.get("capsule_verifies") is False)
    broken = counts.get("mismatch", 0) + counts.get("missing", 0)
    code = 1 if broken or bad or (strict and stale) else 0
    if as_json:
        print(json.dumps({"schema": SCHEMA, "kind": "oms-run-claims-check", "claims": results,
                          "counts": counts, "stale": stale, "unreadable_rows": bad,
                          "verified_capsules": verify}, ensure_ascii=False, sort_keys=True))
        return code
    for result in results:
        recorded = "-" if result["recorded"] is None else fmt(result["recorded"])
        print("%-8s %s  %s: claimed %s, recorded %s (tolerance %s)  capsule %s%s"
              % (result["status"], result["id"], result["metric"], fmt(result["claimed"]), recorded,
                 fmt(result["tolerance"]), result["capsule"],
                 "  @ " + result["where"] if result["where"] else ""))
        for note in result["notes"]:
            print("         note: " + note)
    if bad:
        print("unreadable: %d row(s) in the claims file could not be parsed" % bad)
    summary = ", ".join("%d %s" % (counts[key], key) for key in ("match", "mismatch", "missing") if counts.get(key))
    print("claims: %s%s; recorded values only, nothing was re-run and a match is not a reproduction"
          % (summary or "none recorded", ", %d stale" % stale if stale else ""))
    return code


def claims_list(env, args):
    if args:
        fail("claims list takes no arguments")
    claims, _bad = latest(env["claims"])
    for claim in claims:
        print("%s  %s=%s ±%s  capsule %s%s" % (
            claim["id"], claim.get("metric"), fmt(claim.get("value")), fmt(claim.get("tolerance") or 0),
            claim.get("capsule"), "  @ " + claim["where"] if claim.get("where") else ""))
    return 0


# ---- figures --------------------------------------------------------------

def figure_record(env, args):
    output = script = command = ident = None
    runs = []
    i = 0
    while i < len(args):
        key = args[i]
        if key in ("--output", "--script", "--run", "--command", "--id"):
            if i + 1 >= len(args):
                fail("%s requires a value" % key)
            value = args[i + 1]
            if key == "--output":
                output = value
            elif key == "--script":
                script = value
            elif key == "--run":
                runs.append(value)
            elif key == "--command":
                command = value
            else:
                ident = value
            i += 2
        else:
            fail("unknown figure record argument: %s" % key)
    if not output or not script:
        fail("figure record requires --output FILE and --script FILE")
    if len(runs) > MAX_RUNS:
        fail("at most %d --run inputs" % MAX_RUNS)
    record = {}
    for label, path in (("output", output), ("script", script)):
        relative = inside(env["root"], path)
        if relative is None:
            fail("--%s must be a file inside the repository: %s" % (label, path))
        digest = sha256_file(os.path.join(env["root"], relative))
        if digest is None:
            fail("--%s is not a readable regular file (or is over the hash cap): %s" % (label, path))
        record[label] = relative
        record[label + "_sha256"] = digest
    capsules = []
    for run in runs:
        capsule_id, capsule = capsule_for(run, env["runs"], env["spine"])
        if capsule is None:
            fail("--run %s: no capsule recorded for it" % run)
        if capsule_id not in capsules:
            capsules.append(capsule_id)
    existing = {row["id"] for row in rows(env["figures"])[0]}
    ident = ident or new_id("fig", existing, env["ts"])
    if not NAME_RE.match(ident):
        fail("--id must match [A-Za-z0-9][A-Za-z0-9._-]{0,63}")
    record.update({"schema": SCHEMA, "id": ident, "ts": env["ts"], "agent": env["agent"],
                   "command": one_line(command or "", 500), "capsules": capsules})
    emit(record)
    sys.stderr.write("figure %s: recorded %s from %s (%d input run(s)); nothing was run\n"
                     % (ident, record["output"], record["script"], len(capsules)))


def figure_verify(env, args):
    as_json = False
    only = None
    for arg in args:
        if arg == "--json":
            as_json = True
        elif arg.startswith("-"):
            fail("unknown figure verify argument: %s" % arg)
        elif only is None:
            only = arg
        else:
            fail("figure verify takes at most one figure id")
    figures, bad = latest(env["figures"])
    if only is not None:
        figures = [row for row in figures if row["id"] == only]
        if not figures:
            fail("no such figure: %s" % only)
    results = []
    for figure in figures:
        reasons = []
        for label in ("output", "script"):
            path = figure.get(label)
            if not isinstance(path, str) or inside(env["root"], os.path.join(env["root"], path)) is None:
                reasons.append("%s path is not usable" % label)
                continue
            digest = sha256_file(os.path.join(env["root"], path))
            if digest is None:
                reasons.append("%s is gone or unreadable: %s" % (label, path))
            elif digest != figure.get(label + "_sha256"):
                reasons.append("%s changed since it was recorded: %s" % (label, path))
        for capsule_id in figure.get("capsules") or []:
            if capsule_for(capsule_id, env["runs"], env["spine"])[1] is None:
                reasons.append("input capsule not found: %s" % capsule_id)
        results.append({"id": figure["id"], "output": figure.get("output"), "script": figure.get("script"),
                        "status": "stale" if reasons else "current", "reasons": reasons,
                        "command": figure.get("command") or "", "capsules": figure.get("capsules") or []})
    stale = sum(1 for result in results if result["status"] == "stale")
    code = 1 if stale or bad else 0
    if as_json:
        print(json.dumps({"schema": SCHEMA, "kind": "oms-run-figure-verify", "figures": results,
                          "stale": stale, "unreadable_rows": bad}, ensure_ascii=False, sort_keys=True))
        return code
    for result in results:
        print("%-8s %s  %s  <- %s  (%d input run(s))" % (
            result["status"], result["id"], result["output"], result["script"], len(result["capsules"])))
        for reason in result["reasons"]:
            print("         " + reason)
        if result["reasons"] and result["command"]:
            print("         recorded command (not run): " + result["command"])
    if bad:
        print("unreadable: %d row(s) in the figures file could not be parsed" % bad)
    print("figures: %d current, %d stale; file digests only, no figure was regenerated"
          % (len(results) - stale, stale))
    return code


def main(argv):
    env = {key: os.environ.get("OMS_CLAIMS_" + key.upper(), "") for key in
           ("root", "runs", "spine", "claims", "figures", "ts", "agent", "capsule_tool")}
    group, action, rest = argv[0], argv[1], argv[2:]
    table = {("claims", "add"): claims_add, ("claims", "check"): claims_check, ("claims", "list"): claims_list,
             ("figure", "record"): figure_record, ("figure", "verify"): figure_verify}
    handler = table.get((group, action))
    if handler is None:
        fail("unknown %s action: %s" % (group, action))
    return handler(env, rest) or 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
