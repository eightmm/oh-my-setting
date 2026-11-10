#!/usr/bin/env python3
"""Candidate results ledger: .oms/candidates.jsonl.

Bookkeeping only. It keeps results from different tools and runs attached to
one candidate id the user chose, and points each number at the run capsule,
ledger row or external receipt that produced it. It does not parse or validate
structures and does not check that a score is right. Append-only, schema 1;
the shell wrapper scans rows for sensitive text and appends under the repo's
file lock. Stdlib only.
"""
import argparse
import datetime
import hashlib
import json
import math
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import external_receipts  # noqa: E402

KINDS = ("ligand", "protein", "complex", "pose", "other")
ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$")
TOOL_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/@:+-]{0,63}$")
METRIC_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._/-]{0,63}$")
REF_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}=[^\x00-\x1f\x7f]{1,200}$")
LINK_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$")
NUMBER_RE = re.compile(r"^[+-]?(\d+(\.\d*)?|\.\d+)([eE][+-]?\d+)?$")
TEXT_RE = re.compile(r"^[^\x00-\x08\x0a-\x1f\x7f]*$")
MAX_REFS = 16


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def die(message, code=2):
    print("error: %s" % message, file=sys.stderr)
    sys.exit(code)


def store_path(root):
    return os.environ.get("OMS_CANDIDATES_FILE") or os.path.join(root, ".oms", "candidates.jsonl")


def text_field(value, name, limit=500):
    if value is None:
        return None
    if len(value) > limit or not TEXT_RE.match(value):
        die("%s must be plain single-line text of at most %d characters" % (name, limit))
    return value


def load(path):
    candidates, order, results = {}, [], []
    if not os.path.exists(path):
        return candidates, order, results
    with open(path, encoding="utf-8") as handle:
        for number, line in enumerate(handle, 1):
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                die("%s line %d is not JSON" % (path, number))
            if not isinstance(row, dict) or row.get("type") not in ("candidate", "result") \
                    or not isinstance(row.get("id"), str):
                die("%s line %d is not a candidate row" % (path, number))
            if row["type"] == "candidate":
                if row["id"] not in candidates:
                    order.append(row["id"])
                candidates[row["id"]] = row
            else:
                results.append(row)
    return candidates, order, results


def write_row(args, row):
    if not args.out:
        die("internal: --out is required for writes")
    with open(args.out, "w", encoding="utf-8") as handle:
        handle.write(json.dumps(row, ensure_ascii=False, sort_keys=True) + "\n")


def read_stdin_text():
    return sys.stdin.read().strip()


def cmd_add(args, root, path):
    if not ID_RE.match(args.id):
        die("--id must match %s (your own stable identifier; never parsed as chemistry)"
            % ID_RE.pattern)
    refs = []
    for ref in args.ref or []:
        if not REF_RE.match(ref):
            die("--ref must look like label=value (label letters, digits, _.-), got: %s" % ref[:40])
        refs.append(ref)
    row = {"schema": 1, "type": "candidate", "id": args.id, "kind": args.kind, "ts": now()}
    notes = []
    smiles = args.smiles
    if smiles == "-":
        smiles = read_stdin_text()
    if smiles is not None:
        if not smiles or len(smiles) > 4096 or not TEXT_RE.match(smiles) or smiles != smiles.strip():
            die("--smiles must be one non-empty line of at most 4096 characters")
        refs.append("smiles_sha256=" + hashlib.sha256(smiles.encode("utf-8")).hexdigest())
        row["smiles_length"] = len(smiles)
        if args.store_structure:
            row["smiles"] = smiles
            notes.append("note: --store-structure keeps the SMILES string in %s; do not share that file "
                         "if the structure is unpublished" % os.path.basename(path))
        else:
            notes.append("note: only the sha256 and length of the SMILES were stored, not the string "
                         "(unpublished structures should not sit in a shared log; --store-structure "
                         "keeps it)")
    elif args.store_structure:
        die("--store-structure needs --smiles")
    if len(refs) > MAX_REFS:
        die("at most %d references per candidate" % MAX_REFS)
    if refs:
        row["refs"] = refs
    note = text_field(args.note, "--note")
    if note:
        row["note"] = note
    write_row(args, row)
    print("%s: %s candidate recorded (bookkeeping only; structure and scores are not validated)"
          % (args.id, args.kind))
    for line in notes:
        print(line)
    return 0


def ledger_ids(root):
    path = os.environ.get("LEDGER") or os.path.join(root, "docs", "EXPERIMENTS.jsonl")
    found = set()
    if not os.path.isfile(path):
        return found
    with open(path, encoding="utf-8", errors="replace") as handle:
        for line in handle:
            try:
                row = json.loads(line)
            except ValueError:
                continue
            if isinstance(row, dict) and isinstance(row.get("id"), str):
                found.add(row["id"])
    return found


def resolve_run(root, run_id):
    if not LINK_RE.match(run_id):
        die("--run is not a valid capsule or ledger id: %s" % run_id[:40])
    runs = os.environ.get("OMS_RUNS_DIR") or os.path.join(root, ".oms", "runs")
    if os.path.isfile(os.path.join(runs, run_id, "capsule.json")):
        return "capsule"
    if run_id in ledger_ids(root):
        return "ledger"
    die("--run %s is neither a run capsule (.oms/runs/<id>) nor a run-ledger row id; refusing an "
        "unresolvable link (use --unlinked to record a number without one)" % run_id)


def parse_value(text):
    if not NUMBER_RE.match(text):
        die("--value must be a plain decimal number, got: %s" % text[:40])
    if re.match(r"^[+-]?\d+$", text) and len(text.lstrip("+-")) <= 15:
        return int(text)
    value = float(text)
    if not math.isfinite(value):
        die("--value must be finite")
    return value


def cmd_result(args, root, path):
    candidates, _, _ = load(path)
    if args.id not in candidates:
        die("no such candidate: %s (register it first: oms candidate add --id %s)" % (args.id, args.id))
    if not TOOL_RE.match(args.tool):
        die("--tool must match %s" % TOOL_RE.pattern)
    if not METRIC_RE.match(args.metric):
        die("--metric must match %s" % METRIC_RE.pattern)
    if not (args.run or args.external or args.unlinked):
        die("a result needs --run RUN_OR_CAPSULE_ID or --external RECEIPT_ID so the number points at "
            "how it was produced; pass --unlinked to record it without a link (it is shown as unlinked)")
    if args.unlinked and (args.run or args.external):
        die("--unlinked cannot be combined with --run or --external")
    row = {"schema": 1, "type": "result", "id": args.id, "tool": args.tool, "metric": args.metric,
           "value": parse_value(args.value), "ts": now()}
    unit = text_field(args.unit, "--unit", 32)
    if unit:
        row["unit"] = unit
    conditions = text_field(args.conditions, "--conditions")
    if conditions:
        row["conditions"] = conditions
    if args.run:
        row["run"] = args.run
        row["run_via"] = resolve_run(root, args.run)
    if args.external:
        receipts, _ = external_receipts.load(external_receipts.store_path(root))
        if args.external not in receipts:
            die("--external %s is not a receipt in .oms/external.jsonl (oms external list); refusing an "
                "unresolvable link" % args.external)
        row["external"] = args.external
    if args.higher_is_better:
        row["direction"] = "higher"
    elif args.lower_is_better:
        row["direction"] = "lower"
    write_row(args, row)
    link = "unlinked" if args.unlinked else ", ".join(
        x for x in (("run " + args.run) if args.run else "", ("receipt " + args.external)
                    if args.external else "") if x)
    print("%s: %s/%s = %s%s recorded (%s)" % (args.id, args.tool, args.metric, row["value"],
                                              " " + unit if unit else "", link))
    return 0


# ---- reading side -------------------------------------------------------

def link_key(row, index):
    if row.get("run") or row.get("external"):
        return ("link", row.get("run", ""), row.get("external", ""))
    return ("unlinked", index)


def analyse(results):
    """Mark rows superseded by a later row on the same link, then flag conflicts."""
    entries = []
    latest = {}
    for index, row in enumerate(results):
        group = (row["id"], row["tool"], row["metric"], row.get("conditions", ""))
        key = (group, link_key(row, index))
        entries.append({"row": row, "group": group, "superseded": False, "key": key})
        if key in latest:
            entries[latest[key]]["superseded"] = True
        latest[key] = index
    values = {}
    for entry in entries:
        if not entry["superseded"]:
            values.setdefault(entry["group"], set()).add(float(entry["row"]["value"]))
    conflicts = {group for group, seen in values.items() if len(seen) > 1}
    return entries, conflicts


def fmt(value):
    return json.dumps(value)


def link_text(row):
    parts = []
    if row.get("run"):
        parts.append("run %s (%s)" % (row["run"], row.get("run_via", "?")))
    if row.get("external"):
        parts.append("receipt %s" % row["external"])
    return ", ".join(parts) or "unlinked"


def cmd_show(args, root, path):
    candidates, _, results = load(path)
    cand = candidates.get(args.id)
    if cand is None:
        die("no such candidate: %s" % args.id)
    entries, conflicts = analyse(results)
    mine = [e for e in entries if e["row"]["id"] == args.id]
    if args.json:
        out = {"schema": 1, "candidate": cand, "results": [
            dict(e["row"], link=link_text(e["row"]), superseded=e["superseded"],
                 conflict=e["group"] in conflicts and not e["superseded"]) for e in mine]}
        print(json.dumps(out, ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    print("%s  (%s, added %s)" % (cand["id"], cand.get("kind", "other"), cand.get("ts", "?")))
    for ref in cand.get("refs", []):
        print("  ref %s" % ref)
    if cand.get("smiles_length") is not None and "smiles" not in cand:
        print("  structure string not stored (digest and length only)")
    if cand.get("note"):
        print("  note: %s" % cand["note"])
    if not mine:
        print("no results recorded")
        return 0
    tools = []
    for entry in mine:
        pair = (entry["row"]["tool"], entry["row"]["metric"])
        if pair not in tools:
            tools.append(pair)
    for tool, metric in sorted(tools):
        group = [e for e in mine if (e["row"]["tool"], e["row"]["metric"]) == (tool, metric)]
        print("%s / %s" % (tool, metric))
        for entry in group:
            row = entry["row"]
            line = "  %s%s  %s  %s" % (fmt(row["value"]), " " + row["unit"] if row.get("unit") else "",
                                       link_text(row), row["ts"])
            if row.get("conditions"):
                line += "  conditions: " + row["conditions"]
            if row.get("direction"):
                line += "  (%s is better)" % row["direction"]
            if entry["superseded"]:
                line += "  [superseded by a later row on the same link]"
            print(line)
        bad = sorted({g for g in conflicts if g[0] == args.id and g[1:3] == (tool, metric)},
                     key=lambda g: g[3])
        for g in bad:
            seen = [fmt(e["row"]["value"]) + " (" + link_text(e["row"]) + ")"
                    for e in group if e["group"] == g and not e["superseded"]]
            print("  conflict: same tool, metric and conditions%s, different values: %s; none picked"
                  % (" (" + g[3] + ")" if g[3] else "", " vs ".join(seen)))
    return 0


def build_table(candidates, order, results, metrics):
    entries, conflicts = analyse(results)
    live = [e for e in entries if not e["superseded"]]
    columns = []
    for entry in entries:
        pair = (entry["row"]["tool"], entry["row"]["metric"])
        if (not metrics or pair[1] in metrics) and pair not in columns:
            columns.append(pair)
    columns.sort()
    rows = []
    for cid in sorted(order):
        cells = {}
        for tool, metric in columns:
            mine = [e for e in live if e["row"]["id"] == cid
                    and (e["row"]["tool"], e["row"]["metric"]) == (tool, metric)]
            if not mine:
                continue
            vals = []
            for entry in mine:
                if entry["row"]["value"] not in vals:
                    vals.append(entry["row"]["value"])
            conflict = any(e["group"] in conflicts for e in mine)
            cells[(tool, metric)] = {
                "values": vals, "conflict": conflict,
                "multiple_conditions": len(vals) > 1 and not conflict,
                "unlinked": any(not (e["row"].get("run") or e["row"].get("external")) for e in mine)}
        rows.append((cid, cells))
    directions = {}
    for tool, metric in columns:
        seen = {e["row"].get("direction") for e in live
                if (e["row"]["tool"], e["row"]["metric"]) == (tool, metric)} - {None}
        directions[(tool, metric)] = seen.pop() if len(seen) == 1 else None
    ranking = {"by": None, "reason": "pass exactly one --metric with a stated direction to order rows"}
    if len(metrics) == 1 and len(columns) == 1 and directions[columns[0]]:
        col = columns[0]
        ranking = {"by": "%s:%s" % col, "direction": directions[col], "reason": ""}
        sign = 1 if directions[col] == "lower" else -1
        single = [r for r in rows if col in r[1] and len(r[1][col]["values"]) == 1]
        rest = [r for r in rows if r not in single]
        single.sort(key=lambda r: sign * float(r[1][col]["values"][0]))  # stable: ties stay in id order
        rows = single + rest
    elif len(metrics) == 1 and len(columns) > 1:
        ranking["reason"] = "metric %s is reported by several tools; rows are not ordered across tools" % \
            next(iter(metrics))
    elif len(metrics) == 1 and columns and not directions[columns[0]]:
        ranking["reason"] = "no single direction stated for %s (use --higher-is-better or " \
                            "--lower-is-better when recording results)" % columns[0][1]
    return columns, rows, ranking


def cell_text(cell):
    if cell is None:
        return "-"
    text = "/".join(fmt(v) for v in cell["values"])
    if cell["conflict"]:
        text += " !conflict"
    elif cell["multiple_conditions"]:
        text += " ~conditions"
    if cell["unlinked"]:
        text += " ?unlinked"
    return text


def cmd_table(args, root, path):
    candidates, order, results = load(path)
    metrics = set(args.metric or [])
    columns, rows, ranking = build_table(candidates, order, results, metrics)
    fmt_name = "json" if args.json else args.format
    names = ["%s:%s" % c for c in columns]
    if fmt_name == "json":
        out = {"schema": 1, "columns": [{"tool": t, "metric": m} for t, m in columns],
               "rows": [{"id": cid, "cells": {"%s:%s" % c: v for c, v in cells.items()}}
                        for cid, cells in rows],
               "ranking": ranking}
        print(json.dumps(out, ensure_ascii=False, indent=2, sort_keys=True))
        return 0
    if not order:
        print("no candidates (%s)" % path)
        return 0
    header = ["candidate"] + names
    body = [[cid] + [cell_text(cells.get(c)) for c in columns] for cid, cells in rows]
    if fmt_name == "tsv":
        for line in [header] + body:
            print("\t".join(line))
        return 0
    print("| " + " | ".join(header) + " |")
    print("|" + "|".join("---" for _ in header) + "|")
    for line in body:
        print("| " + " | ".join(line) + " |")
    if ranking["by"]:
        print("\nordered by %s (%s is better); rows without exactly one value follow in id order"
              % (ranking["by"], ranking["direction"]))
    else:
        print("\nnot ranked: %s" % ranking["reason"])
    print("markers: !conflict same conditions, different values (all shown, none picked); "
          "~conditions values differ by conditions; ?unlinked no run or receipt")
    return 0


def main(argv):
    parser = argparse.ArgumentParser(prog="candidate")
    parser.add_argument("--root", required=True)
    parser.add_argument("--out")
    sub = parser.add_subparsers(dest="cmd", required=True)
    add = sub.add_parser("add")
    add.add_argument("--id", required=True)
    add.add_argument("--kind", choices=KINDS, default="other")
    add.add_argument("--ref", action="append")
    add.add_argument("--smiles")
    add.add_argument("--store-structure", action="store_true")
    add.add_argument("--note")
    res = sub.add_parser("result")
    res.add_argument("--id", required=True)
    res.add_argument("--tool", required=True)
    res.add_argument("--metric", required=True)
    res.add_argument("--value", required=True)
    res.add_argument("--unit")
    res.add_argument("--run")
    res.add_argument("--external")
    res.add_argument("--conditions")
    res.add_argument("--unlinked", action="store_true")
    direction = res.add_mutually_exclusive_group()
    direction.add_argument("--higher-is-better", action="store_true")
    direction.add_argument("--lower-is-better", action="store_true")
    show = sub.add_parser("show")
    show.add_argument("id")
    show.add_argument("--json", action="store_true")
    table = sub.add_parser("table")
    table.add_argument("--metric", action="append")
    table.add_argument("--json", action="store_true")
    table.add_argument("--format", choices=("md", "tsv"), default="md")
    args = parser.parse_args(argv)
    path = store_path(args.root)
    return {"add": cmd_add, "result": cmd_result, "show": cmd_show, "table": cmd_table}[args.cmd](
        args, args.root, path)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
