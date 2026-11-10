#!/usr/bin/env bash
set -euo pipefail

# Static check of one Slurm job script against the cluster snapshot's QOS.
# A script's preemption handling used to be checked by reading prose; this
# reads its #SBATCH lines and the QOS table the private cluster snapshot
# (`oms snapshot --cluster`) already holds. It reads those two files, never
# runs the script and never calls Slurm.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SNAPSHOT="${OH_MY_SETTING_SLURM_REF:-$ROOT/local/slurm.md}"
TEMPLATE="$ROOT/templates/slurm-job.sh"

usage() {
  cat <<'EOF'
Usage: slurm-lint.sh SCRIPT [--snapshot FILE]
       slurm-lint.sh --preview SCRIPT [--snapshot FILE]
       slurm-lint.sh --template

Static check of a Slurm batch script. Nothing is submitted or executed.

Warnings:
  time      no --time: the queue's default or maximum decides.
  log       --output/--error is not job-specific (no %j; an array needs %a
            beside %A).
  array     --array has no concurrency limit (%N).
  requeue   the job's QOS preempts by REQUEUE but the script does not ask for
            --requeue (or asks for --no-requeue).
  signal    that QOS requeues but the script has neither --signal nor a trap,
            or traps in the batch shell while --signal lacks the B: prefix.
  maxwall   --time is longer than the QOS MaxWall.

The QOS is the script's --qos, else the snapshot's "QOS default". Its
PreemptMode and MaxWall come from the snapshot's QOS table (default
local/slurm.md, or OH_MY_SETTING_SLURM_REF, or --snapshot). Without a snapshot
or a known QOS the preemption checks report "unknown", never a pass.

--preview prints what the script asks for, from its #SBATCH lines only: GPUs,
wall time, array size, and the upper-bound GPU-hours (GPUs x wall x array
tasks, as if every task ran to its limit). It sets those beside the per-job
and group GPU limits the snapshot records (QOS and association rows, with
the snapshot's age). A quantity the script or snapshot does not state is
shown as unknown, never guessed. Nothing is submitted, and Slurm's own
--test-only is not used. Exit 0 unless the script or arguments are unusable.

--template prints a preemption-aware starting script (templates/slurm-job.sh).

Exit: 0 no warning, 1 at least one warning, 2 usage or unreadable script.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

SCRIPT=""
PREVIEW=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
    --preview) PREVIEW=1; shift ;;
    --template)
      [ "$#" -eq 1 ] && [ -z "$SCRIPT" ] || fail "--template takes no other argument"
      [ -f "$TEMPLATE" ] || fail "template missing: $TEMPLATE"
      cat "$TEMPLATE"
      exit 0
      ;;
    --snapshot) [ "$#" -ge 2 ] || fail "--snapshot requires a file"; SNAPSHOT="$2"; shift 2 ;;
    -*) fail "unknown argument: $1" ;;
    *) [ -z "$SCRIPT" ] || fail "one script at a time"; SCRIPT="$1"; shift ;;
  esac
done
[ -n "$SCRIPT" ] || { usage >&2; exit 2; }
[ -f "$SCRIPT" ] && [ -r "$SCRIPT" ] || fail "cannot read script: $SCRIPT"
command -v python3 >/dev/null 2>&1 || fail "python3 is required"

python3 - "$SCRIPT" "$SNAPSHOT" "$PREVIEW" <<'PY'
import re
import shlex
import sys
import time

script, snapshot, preview_mode = sys.argv[1:4]
SHORT = {"t": "time", "o": "output", "e": "error", "a": "array", "q": "qos",
         "N": "nodes", "n": "ntasks", "G": "gpus", "p": "partition"}
FLAGS = {"requeue", "no-requeue"}

try:
    with open(script, encoding="utf-8", errors="replace") as handle:
        lines = handle.read().splitlines()
except OSError as error:
    print("error: cannot read script: %s" % error.strerror, file=sys.stderr)
    sys.exit(2)

# sbatch reads #SBATCH lines up to the first command; later ones are comments.
options, body = {}, []
header = True
for line in lines:
    text = line.strip()
    if header and text.startswith("#SBATCH"):
        try:
            words = shlex.split(text[len("#SBATCH"):], comments=True)
        except ValueError:
            continue
        i = 0
        while i < len(words):
            word = words[i]
            if word.startswith("--"):
                name, _, value = word[2:].partition("=")
                if "=" not in word and name not in FLAGS and i + 1 < len(words) and not words[i + 1].startswith("-"):
                    value, i = words[i + 1], i + 1
                options[name] = value
            elif word.startswith("-") and len(word) >= 2:
                name, value = SHORT.get(word[1], word[1]), word[2:]
                if not value and i + 1 < len(words) and not words[i + 1].startswith("-"):
                    value, i = words[i + 1], i + 1
                options[name] = value
            i += 1
        continue
    if text and not text.startswith("#"):
        header = False
        body.append(text)


def seconds(value):
    """A Slurm time limit in seconds; None when unlimited or not understood."""
    value = (value or "").strip()
    if not value or value.upper() in ("UNLIMITED", "INFINITE", "-1"):
        return None
    days, _, rest = value.rpartition("-")
    parts = rest.split(":")
    if not all(part.isdigit() for part in parts) or (days and not days.isdigit()) or len(parts) > 3:
        return None
    numbers = [int(part) for part in parts]
    if days:
        hours, minutes, secs = (numbers + [0, 0])[:3]
    elif len(numbers) == 1:
        hours, minutes, secs = 0, numbers[0], 0
    elif len(numbers) == 2:
        hours, minutes, secs = 0, numbers[0], numbers[1]
    else:
        hours, minutes, secs = numbers
    return int(days or 0) * 86400 + hours * 3600 + minutes * 60 + secs


def qos_table(path):
    """({name: row}, default QOS, why not) from the snapshot's QOS section."""
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            text = handle.read().splitlines()
    except OSError:
        return {}, "", "no cluster snapshot; run: oms snapshot --cluster"
    rows, columns, inside, default = {}, None, False, ""
    for line in text:
        if line.startswith("- QOS default:"):
            default = line.split(":", 1)[1].strip()
            if default.startswith("("):
                default = ""
        if line.startswith("## "):
            inside = line.strip() == "## QOS"
            columns = None
            continue
        if not inside or line.startswith("```") or "|" not in line:
            continue
        cells = line.split("|")
        if columns is None:
            columns = cells
        elif len(cells) == len(columns):
            rows[cells[0]] = dict(zip(columns, cells))
    return rows, default, "" if rows else "the snapshot has no QOS table"


def section_rows(path, prefix):
    """([row dicts], Generated stamp) for the snapshot section whose heading starts with prefix."""
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            text = handle.read().splitlines()
    except OSError:
        return [], ""
    found, columns, inside, stamp = [], None, False, ""
    for line in text:
        if line.startswith("Generated:"):
            stamp = line.split(":", 1)[1].strip()
        if line.startswith("## "):
            inside, columns = line.startswith(prefix), None
            continue
        if not inside or line.startswith("```") or "|" not in line:
            continue
        cells = line.split("|")
        if columns is None:
            columns = cells
        elif len(cells) == len(columns):
            found.append(dict(zip(columns, cells)))
    return found, stamp


def tres_gpus(value):
    """The GPU count in a TRES string such as cpu=64,gres/gpu=4; None when it names none."""
    bare, typed = None, 0
    for part in (value or "").split(","):
        match = re.fullmatch(r"\s*gres/gpu(:[^=]+)?=(\d+)\s*", part)
        if match and match.group(1):
            typed += int(match.group(2))
        elif match:
            bare = int(match.group(2))
    return bare if bare is not None else (typed or None)


def first_int(value, upper=False):
    numbers = re.findall(r"\d+", value or "")
    return int(numbers[-1 if upper else 0]) if numbers else None


def request_gpus():
    """(GPU count or None when it cannot be told, how it was read)."""
    nodes = first_int(options.get("nodes"), upper=True) or 1
    if "ntasks" in options:
        tasks = first_int(options["ntasks"])
    elif "ntasks-per-node" in options:
        per = first_int(options["ntasks-per-node"])
        tasks = None if per is None else per * nodes
    else:
        tasks = nodes
    if "gpus" in options:
        count = first_int(options["gpus"])
        return count, "--gpus=%s" % options["gpus"]
    if "gpus-per-node" in options:
        count = first_int(options["gpus-per-node"])
        return (None if count is None else count * nodes), "--gpus-per-node=%s x %d node(s)" % (options["gpus-per-node"], nodes)
    if "gpus-per-task" in options:
        count = first_int(options["gpus-per-task"])
        if count is None or tasks is None:
            return None, "--gpus-per-task=%s, task count not stated" % options["gpus-per-task"]
        return count * tasks, "--gpus-per-task=%s x %d task(s)" % (options["gpus-per-task"], tasks)
    if "gres" in options:
        per_node = 0
        for item in re.sub(r"\([^)]*\)", "", options["gres"]).split(","):
            parts = item.split(":")
            if parts[0] == "gpu":
                per_node += int(parts[-1]) if len(parts) > 1 and parts[-1].isdigit() else 1
        return per_node * nodes, "--gres=%s x %d node(s)" % (options["gres"], nodes)
    return 0, "no GPU option in the #SBATCH lines"


def array_size(spec):
    """(task count, concurrency limit or None); (None, None) when the spec is not understood."""
    body, _, limit = spec.partition("%")
    total = 0
    for part in body.split(","):
        match = re.fullmatch(r"(\d+)(?:-(\d+)(?::(\d+))?)?", part.strip())
        if not match:
            return None, None
        low = int(match.group(1))
        high = int(match.group(2)) if match.group(2) else low
        step = int(match.group(3) or 1)
        if high < low or step < 1:
            return None, None
        total += (high - low) // step + 1
    return total, int(limit) if limit.isdigit() else None


def age_text(stamp):
    try:
        import calendar
        taken = calendar.timegm(time.strptime(stamp, "%Y-%m-%dT%H:%M:%SZ"))
    except (ValueError, OverflowError):
        return "generation time unreadable"
    days, rest = divmod(max(0, int(time.time() - taken)), 86400)
    text = "%s, %dd %dh old" % (stamp, days, rest // 3600)
    return text + (" - older than 7 days; refresh with: oms snapshot --cluster" if days >= 7 else "")


def preview():
    gpus, how = request_gpus()
    wall = seconds(options.get("time"))
    tasks, concurrent, array_note = 1, None, "no --array: one job"
    if "array" in options:
        tasks, concurrent = array_size(options["array"])
        array_note = "--array=%s" % options["array"] if tasks is not None else "--array=%s not understood" % options["array"]
    print("slurm-preview: %s (read from #SBATCH lines only; nothing was submitted, Slurm --test-only was not used)" % script)
    qos_rows, default_qos, why = qos_table(snapshot)
    assoc, stamp = section_rows(snapshot, "## Associations")
    if not qos_rows and not assoc and not stamp:
        print("snapshot: none readable at %s; run: oms snapshot --cluster" % snapshot)
    else:
        print("snapshot: %s (%s)" % (snapshot, age_text(stamp) if stamp else "no Generated line"))
    print("")
    print("request")
    print("  GPUs per array task          %s  (%s)" % ("unknown" if gpus is None else gpus, how))
    print("  wall time                    %s" % ("unknown (no --time, or not understood)" if wall is None
                                                else "%s (%.2f h)" % (options["time"], wall / 3600.0)))
    print("  array tasks                  %s  (%s)" % ("unknown" if tasks is None else tasks, array_note))
    known = gpus is not None and wall is not None and tasks is not None
    print("  GPU-hours, upper bound       %s" % ("%.1f  (GPUs x wall x tasks, as if every task ran to its limit)"
                                                % (gpus * wall / 3600.0 * tasks) if known else "unknown"))
    peak = None if gpus is None or tasks is None else gpus * (min(tasks, concurrent) if concurrent else tasks)
    print("  GPUs held at once, peak      %s%s" % ("unknown" if peak is None else peak,
                                                   "" if concurrent or not tasks or tasks < 2 else "  (no %N limit: every task may run together)"))
    print("")
    print("limits recorded in the snapshot (a limit not listed here is not recorded, which is not the same as no limit)")
    qos = (options.get("qos") or default_qos).split(",")[0].strip()
    qos_row = qos_rows.get(qos) if qos else None
    checks = []
    if qos_row is not None:
        checks += [("QOS %s MaxTRESPJ" % qos, "per job", tres_gpus(qos_row.get("MaxTRESPJ"))),
                   ("QOS %s MaxTRESPU" % qos, "at once", tres_gpus(qos_row.get("MaxTRESPU"))),
                   ("QOS %s GrpTRES" % qos, "at once", tres_gpus(qos_row.get("GrpTRES")))]
    else:
        print("  QOS: %s" % ("none named and no snapshot default" if not qos else "%s is not in the snapshot" % qos))
    part = options.get("partition")
    mine = [r for r in assoc if not part or r.get("Partition") in ("", part)]
    for column, kind in (("MaxTRESPJ", "per job"), ("MaxTRES", "per job"), ("GrpTRES", "at once")):
        values = [v for v in (tres_gpus(r.get(column)) for r in mine) if v is not None]
        if values:
            checks.append(("association %s (%d row(s), lowest)" % (column, len(mine)), kind, min(values)))
    for label, kind, limit in checks:
        asked = gpus if kind == "per job" else peak
        if limit is None:
            continue  # the row records no GPU limit; saying so for each column is noise
        if asked is None:
            verdict = "request unknown"
        else:
            verdict = "within" if asked <= limit else "OVER by %d" % (asked - limit)
        print("  %-44s gres/gpu=%-4s %s asks %-4s %s" % (label, "-" if limit is None else limit, kind,
                                                       "?" if asked is None else asked, verdict))
    cap = seconds((qos_row or {}).get("MaxWall"))
    if cap is not None and wall is not None:
        print("  %-44s %-9s wall asks %s  %s" % ("QOS %s MaxWall" % qos, qos_row["MaxWall"], options["time"],
                                                 "within" if wall <= cap else "OVER"))
    print("  GPU-hour budgets (GrpTRESMins) are not in the snapshot, so the GPU-hours above have no limit to compare with.")


if preview_mode == "1":
    preview()
    sys.exit(0)

warnings, notes = [], []
if "time" not in options:
    warnings.append(("time", "no --time: the queue's default or maximum decides how long this runs"))
array = options.get("array")
for key in ("output", "error"):
    target = options.get(key)
    if not target:
        continue
    if not re.search(r"%[0-9]*[jJ]", target) and not re.search(r"%[0-9]*A", target):
        warnings.append(("log", "--%s %s is not job-specific: add %%j so runs do not overwrite each other" % (key, target)))
    elif array is not None and re.search(r"%[0-9]*A", target) and not re.search(r"%[0-9]*[ajJ]", target):
        warnings.append(("log", "--%s %s is shared by every array task: add %%a" % (key, target)))
if array is not None and "%" not in array:
    warnings.append(("array", "--array %s has no concurrency limit: add %%N (for example %s%%8)" % (array, array)))

rows, default, why = qos_table(snapshot)
qos = (options.get("qos") or default).split(",")[0].strip()
row = rows.get(qos)
if why:
    notes.append("preemption: unknown (%s)" % why)
elif not qos:
    notes.append("preemption: unknown (the script names no --qos and the snapshot has no QOS default)")
elif row is None:
    notes.append("preemption: unknown (QOS %s is not in the snapshot)" % qos)
else:
    mode = (row.get("PreemptMode") or "").upper()
    if "PreemptMode" not in row:
        notes.append("preemption: unknown (the snapshot's QOS table has no PreemptMode column)")
    elif "REQUEUE" in mode:
        trapped = any(re.match(r"trap\b", text) for text in body)
        signal = options.get("signal")
        if "no-requeue" in options:
            warnings.append(("requeue", "QOS %s preempts by REQUEUE but the script says --no-requeue: "
                             "a preempted job ends instead of returning to the queue" % qos))
        elif "requeue" not in options:
            warnings.append(("requeue", "QOS %s preempts by REQUEUE but the script does not ask for --requeue: whether "
                             "it returns to the queue then depends on the cluster's JobRequeue, which the snapshot does not record" % qos))
        if signal is None and not trapped:
            warnings.append(("signal", "QOS %s requeues, and the script has neither --signal nor a trap: "
                             "the run gets no warning to write a checkpoint" % qos))
        elif signal is not None and trapped and not signal.upper().startswith("B:"):
            warnings.append(("signal", "--signal=%s reaches the job steps only; the trap in this script needs the B: prefix "
                             "(--signal=B:%s) to run" % (signal, signal)))
        elif signal is None and trapped:
            notes.append("signal: the script traps but sets no --signal, so only the scheduler's final TERM arrives")
    else:
        notes.append("preemption: QOS %s PreemptMode is %s; no requeue check applies" % (qos, mode or "not set"))
    limit, asked = seconds(row.get("MaxWall")), seconds(options.get("time"))
    unlimited = (options.get("time") or "").strip().upper() in ("0", "UNLIMITED", "INFINITE", "-1")
    if limit not in (None, 0) and ((asked is not None and asked > limit) or unlimited):
        warnings.append(("maxwall", "--time %s is longer than QOS %s MaxWall %s" % (options["time"], qos, row["MaxWall"])))

print("slurm-lint: %s: %d warning(s); static check, nothing was submitted or run" % (script, len(warnings)))
for ident, text in warnings:
    print("warn: %s: %s" % (ident, text))
for text in notes:
    print("note: %s" % text)
sys.exit(1 if warnings else 0)
PY
