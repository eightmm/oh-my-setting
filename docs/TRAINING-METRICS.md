# Recorded training metric diagnostics

`oms job-digest --metrics FILE` adds a bounded, read-only training-history view.
It can stand alone or accompany the existing logfile/job-ID digest. There is no
training launch, job submission, cancellation, retry, model call or state write.
The metadata reader is shared with the workflow-inspection implementation.

```bash
oms job-digest --metrics metrics.jsonl --metrics-json
oms job-digest train.log --metrics metrics.jsonl --metric train/loss --metric-direction minimize
oms job-digest --metrics dvclive/plots/metrics/loss.tsv --metrics-format dvclive
```

## Input and output contracts

The file is explicitly selected and must be an owned, single-link, regular file
inside the current Git repository (or current directory outside Git). Source
paths from rows are never followed. No provider home is searched. Shared
no-follow descriptor reads reject symlinks, traversal, special files and detected
concurrent replacement/modification. A currently changing file can therefore
require a later manual observation, rather than returning a mixed snapshot.

JSONL records have `step` (nonnegative integer), `name` (bounded metric identifier),
`value` (number), optional `ts` (epoch seconds or timezone-bearing ISO-8601), and
optional `run_id` (explicit identity). Different run IDs never share a series.
A missing run ID is unrecorded, not guessed from the filename or job number.

NaN/infinity values, including named strings from training logs, become a null
value with an explicit non-finite state. Invalid rows are counted; they are not
zero-valued observations. Only newline-terminated rows are processed; a final
partial row is counted as an incomplete tail. Duplicate JSON keys are invalid.

The `dvclive` format is one TSV history with `step`, one metric column and an
optional `timestamp` column in milliseconds. It is not DVCLive's `metrics.json`
latest-value summary. See the official [log_metric format](https://doc.dvc.org/dvclive/live/log_metric)
and [file layout](https://doc.dvc.org/dvclive/how-it-works). The adapter uses the
stdlib only; installing or running DVCLive is not required. Real package-generated
histories have not been exercised by this change's synthetic tests.

The JSON output is schema 1, kind `oms-training-metrics`. For each series it gives
latest processed step/value, retained finite minimum/maximum, non-finite counts,
duplicate steps, step/timestamp reversals, and a recent endpoint-change-per-step.
This slope is descriptive, not a fitted forecast or significance estimate.
`--metric-direction minimize|maximize` requires exact `--metric NAME`; otherwise
the direction is `observe` and there is no chosen best value.

A repeated current step replaces its earlier value. A decreasing step starts a
new observed monotone segment: it might be a restart or late data, so it is not
asserted to be a new run. Extrema/trend cover only retained points in that segment.
The numbers are not the best of the entire experiment.

## Freshness, bounds and incomplete histories

`--stale-after S` accepts 1–604800 seconds (default 600). The latest processed
metric's timestamp gives `recent-record`, `late-record`, `clock-skew` or `unknown`.
No mtime substitutes for missing/naive/invalid timestamps. Timestamp reversal
makes freshness unknown. A late record does not prove that training is stalled,
failed or even still running. Nothing acts on the label automatically.

The read budget is 4 MiB. Large files contribute a contiguous suffix only; the
head is used only for a TSV header. The unread gap is never bridged to compute a
trend. The parser accepts at most 50,000 rows, 64 series, 2,048 retained distinct
steps per series and 65,536 bytes per row. It reports discarded prefixes, partial
tails, unprocessed bytes, invalid/filtered rows, series limits and point eviction.
`latest` always means latest **processed** record, not a guarantee of the newest
record in a capped window. The internal trend window defaults to 20 points.

`--metrics-json` is metrics-only and cannot be combined with a log/job operand or
`--max-bytes`. Text may use the existing whole-line byte cap. Other metrics flags
require `--metrics`. The original log/job digest without metrics is unchanged.

Successful observation exits 0 even when it reports NaN, late data or invalid
rows. Invalid options, unreadable files and unsupported TSV headers exit 2.
The existing job wait timeout remains 124. None of these is a training acceptance
verdict. Names and numeric metrics remain private metadata, not safe-to-publish
content; raw record bodies are not returned.

## Tests and rollback

`tests/test_oms_runtime_training_metrics.py` is discovered by the existing
runtime-core suite. It tests finite/non-finite values, explicit runs, correction,
restart ambiguity, timestamp zones, history gaps and bounds, safe reads, immutable
files, and the actual `job-digest.sh` metrics-only/combined entrypoint. No new CI
lane is added. Full registered suites, native Slurm/providers and macOS/WSL2 are
separate checks; module or shell-fixture success is not live training validation.

Revert the scoped commit to remove this optional view. No records are migrated
because neither history nor OMS state is written.
