"""Paste-ready tables of per-seed-group aggregates for `run-ledger table`. Read-only.

Rows are selected and grouped by the same functions `run-ledger top` uses
(run_ledger_metrics.comparable and group_summaries); this module only ranks
and formats what they return.
"""

import json
import math
import os
import sys

import run_ledger_metrics as contract_lib

SIGNIFICANT = 4
FORMATS = ("md", "tsv", "tex")
TEX_ESCAPES = {"\\": r"\textbackslash{}", "_": r"\_", "%": r"\%", "&": r"\&", "#": r"\#", "$": r"\$",
               "{": r"\{", "}": r"\}", "~": r"\textasciitilde{}", "^": r"\textasciicircum{}"}


def number(value, fmt):
    """Every number is printed to SIGNIFICANT significant digits, trailing zeros kept (0.4 -> 0.4000)."""
    text = "%#.*g" % (SIGNIFICANT, value)
    if "e" in text:
        mantissa, exponent = text.split("e")
        text = {"tex": r"%s\times10^{%d}"}.get(fmt, "%se%d") % (mantissa, int(exponent))
    return text


def tex_text(text):
    return "".join(TEX_ESCAPES.get(ch, ch) for ch in text)


def header_comment(metric, direction):
    return ("%s: mean ± stdev over seeds, ci95 = bootstrap 95%% interval, every number rounded to %d significant "
            "digits; ranked %s first; one-seed groups are listed last, unranked and never marked best"
            % (metric, SIGNIFICANT, "highest" if direction == "max" else "lowest"))


def cells(group, rank, best, fmt):
    """The printable cells of one group: rank, group, value, ci95, n, note."""
    _, count, mean, stdev, ci, _ = group
    name = group[0][:60]
    if count == 1:
        value, interval, note, rank = number(mean, fmt), "-", "single seed", "-"
    else:
        value = "%s ± %s" % (number(mean, fmt), number(stdev, fmt))
        interval = "[%s, %s]" % (number(ci[0], fmt), number(ci[1], fmt))
        note, rank = "", str(rank)
    if fmt == "tex":
        value = "$%s$" % value.replace("±", r"\pm") if ("±" in value or "times" in value) else value
        interval = "$%s$" % interval if "times" in interval else interval
        name = tex_text(name)
        value = r"\textbf{%s}" % value if best else value
    elif fmt == "md":
        name = name.replace("|", "\\|")
        value = "**%s**" % value if best else value
    return [rank, name, value, interval, str(count), note]


def render(metric, direction, groups, fmt):
    """Lines of one table. Groups arrive ranked; replicated groups first, single seeds after."""
    replicated = [g for g in groups if g[1] > 1]
    singles = [g for g in groups if g[1] == 1]
    comment = header_comment(metric, direction)
    # Bold the best only when there is another replicated group to be better than.
    rows = [cells(g, i, i == 1 and len(replicated) > 1, fmt) for i, g in enumerate(replicated, 1)]
    rows += [cells(g, 0, False, fmt) for g in singles]
    head = ["#", "group", "%s (mean ± stdev)" % metric, "ci95", "n", "note"]
    if fmt == "tsv":
        return ["# " + comment, "\t".join(head)] + ["\t".join(row) for row in rows]
    if fmt == "md":
        head = [cell.replace("|", "\\|") for cell in head]
        return (["<!-- " + comment + " -->", "| " + " | ".join(head) + " |", "|---:|---|---|---|---:|---|"]
                + ["| " + " | ".join(row) + " |" for row in rows])
    head = [r"\#", "group", "%s (mean $\\pm$ stdev)" % tex_text(metric), "ci95", "$n$", "note"]
    return (["% " + comment, r"\begin{tabular}{rlllrl}", r"\hline",
             " & ".join(head) + r" \\", r"\hline"]
            + [" & ".join(row) + r" \\" for row in rows] + [r"\hline", r"\end{tabular}"])


def one_metric(ledger, root, metric, direction, include_all, mixed, fmt, out, err):
    """Print one metric's table; the exit code `run-ledger top` would use when there is nothing to print."""
    try:
        declared = contract_lib.load_contract(contract_lib.contract_path(ledger)).get(metric)
    except contract_lib.ContractError as error:
        err.write("error: metric contract unreadable (%s); fix it or move it aside\n" % error)
        return 2
    current = contract_lib.metric_record(declared, root) if declared else None
    if declared and direction and direction != declared["direction"]:
        err.write("warning: the metric contract declares %s for %s; ranking --%s as asked\n"
                  % (declared["direction"], metric, direction))
    direction = direction or (declared["direction"] if declared else "max")
    rows = []
    for line in open(ledger, encoding="utf-8", errors="replace"):
        try:
            row = json.loads(line)
        except ValueError:
            continue
        value = (row.get("metrics") or {}).get(metric) if isinstance(row, dict) else None
        if not contract_lib.numeric(value) or (not include_all and row.get("exit") != 0):
            continue
        rows.append((value, row))
    if not rows:
        err.write("table: no ledger rows with a finite numeric metric %r%s\n"
                  % (metric, "" if include_all else " and exit==0 (use --all to include failed runs)"))
        return 3
    if mixed:
        if current is not None or len({contract_lib.row_digest(r, metric) for _, r in rows}) > 1:
            err.write("warning: --mixed tabulates runs measured under different metric contracts together\n")
    else:
        rows, left = contract_lib.comparable(rows, metric, current)
        detail = "; ".join("%d run(s): %s" % (count, reason) for reason, count in sorted(left.items()))
        if not rows:
            err.write("table: not comparable: %s runs do not share one metric contract (%s); "
                      "--mixed tabulates them anyway\n" % (metric, detail))
            return 4
        if left:
            err.write("table: left out as not comparable under today's contract for %s: %s\n" % (metric, detail))
    groups, loose = contract_lib.group_summaries(rows, metric)
    if not groups:
        err.write("table: no run with a seed and group for %s (record with --seed N, or a command carrying "
                  "--seed N)\n" % metric)
        return 3
    groups.sort(key=lambda g: g[2], reverse=(direction == "max"))
    out.write("\n".join(render(metric, direction, groups, fmt)) + "\n")
    if loose:
        err.write("table: %d run(s) without a seed are not grouped\n" % loose)
    return 0


def main(ledger, root, metric, direction, include_all, mixed, fmt, out=None, err=None):
    out, err = out or sys.stdout, err or sys.stderr
    if metric:
        return one_metric(ledger, root, metric, direction, include_all, mixed, fmt, out, err)
    try:
        declared = sorted(contract_lib.load_contract(contract_lib.contract_path(ledger)))
    except contract_lib.ContractError as error:
        err.write("error: metric contract unreadable (%s); fix it or move it aside\n" % error)
        return 2
    if not declared:
        err.write("error: table requires --metric KEY (no metric contract declares any)\n")
        return 2
    printed = 0
    for index, name in enumerate(declared):
        if printed and index:
            out.write("\n")
        if one_metric(ledger, root, name, direction, include_all, mixed, fmt, out, err) == 0:
            printed += 1
    return 0 if printed else 3
