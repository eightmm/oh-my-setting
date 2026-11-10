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

--template prints a preemption-aware starting script (templates/slurm-job.sh).

Exit: 0 no warning, 1 at least one warning, 2 usage or unreadable script.
EOF
}

fail() {
  echo "error: $*" >&2
  exit 2
}

SCRIPT=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -h|--help) usage; exit 0 ;;
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

python3 - "$SCRIPT" "$SNAPSHOT" <<'PY'
import re
import shlex
import sys

script, snapshot = sys.argv[1:3]
SHORT = {"t": "time", "o": "output", "e": "error", "a": "array", "q": "qos"}
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
    if limit is not None and asked is not None and asked > limit:
        warnings.append(("maxwall", "--time %s is longer than QOS %s MaxWall %s" % (options["time"], qos, row["MaxWall"])))

print("slurm-lint: %s: %d warning(s); static check, nothing was submitted or run" % (script, len(warnings)))
for ident, text in warnings:
    print("warn: %s: %s" % (ident, text))
for text in notes:
    print("note: %s" % text)
sys.exit(1 if warnings else 0)
PY
