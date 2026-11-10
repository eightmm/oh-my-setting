# Immutable study workflows

`oms study` executes explicit local CLI wrappers over a frozen suite, evaluates
existing outputs again, identifies uncovered planned cells, preserves correction
history, and binds named baselines. Its JSON is independent of run-ledger,
capsule, claim, figure, and candidate identity schemas.

The default implementation uses the Python standard library. It never selects
a model, starts a provider CLI, submits a scheduler job, or installs a dependency
on its own. A supplied wrapper can call a real model or application; the exact
argv is the execution authority. These programs are trusted local processes,
not an OS sandbox. Their environment and undeclared imports are not frozen.

## Controlled suites and coverage

An executable, offline example is in [examples/study/suite.json](examples/study/suite.json),
with a [suite input schema](examples/study/suite.schema.json).
Each suite contains immutable cases, optional fixture files, named variants,
explicit repeat IDs, one evaluator, and metric directions. Every variant runs
the same cases and expected outputs with the same evaluator. `base_id` is a
human label; `base_sha256` binds the actual case and fixture snapshot.

```bash
oms study suite create --spec docs/examples/study/suite.json --id arithmetic
oms study suite coverage --suite arithmetic --pretty
oms study suite run --suite arithmetic --limit 2 --id first
oms study suite run --suite arithmetic --missing --id remaining
oms study show --id remaining --pretty
```

Coverage enumerates the complete case × variant × repeat plan. It returns
`missing` cells with stable `cell_id` values, completed counts, failed attempts,
duplicate successful attempts, and unfinished batch IDs. `--cell CELL_ID` may
be repeated to execute a precise selection; `--missing` selects only cells
without a successful result in this exact immutable suite. Re-evaluation passes
do not count as new generation coverage. A duplicate attempt never increases
the completed-cell count. Concurrent operators can still select the same missing
cell; the resulting duplicate is reported, not hidden. This is coverage tracking,
not a distributed execution lease.

`--missing` works across earlier batches. Each run command emits its own result
batch, so pairwise comparison of one partial batch reports its unpaired cases.
For a complete paired benchmark, execute the complete plan in one batch:

```bash
oms study suite run --suite arithmetic --id benchmark
oms study compare --results .oms/studies/results/benchmark.json \
  --left baseline --right offset-one --pretty
```

The comparison joins case ID and repeat ID, reports right-minus-left deltas,
per-metric wins, ties and mean paired differences, and lists incomplete pairs.
Duplicate case/variant/repeat observations are refused: select one attempt first.
Recorded base, suite and evaluator digests establish the supplied controls;
external result files without them are labeled `controls.recorded: false`.
These are descriptive differences, not statistical significance or causality.
The existing preregistered experiment contracts remain the separate surface for
noise bands, bootstrap intervals and two-arm acceptance decisions.

### Wrapper contract

A command is a JSON object with `argv`, optional `files`, and
`timeout_seconds` (1–3600, default 60). For example:

```json
{
  "argv": ["python3", "{file:wrapper.py}", "{input}", "{output}"],
  "files": ["wrapper.py"],
  "timeout_seconds": 60
}
```

OMS invokes the argument vector directly without shell interpolation. Every
placeholder occupies an entire token. `{file:PATH}` resolves a declared,
byte-hashed tool file copied into the run directory. The executable itself is
resolved and byte-hashed at registration, then checked before and after use.
Declared tool files and fixtures are also verified; changes to the original
working copy after registration do not alter the frozen suite.

| Token | Generation wrapper | Evaluator |
| --- | --- | --- |
| `{input}` | Frozen case JSON path | The same case JSON path |
| `{output}` | Prediction output path | Metrics output path |
| `{prediction}` | Prediction path | Existing prediction path |
| `{params}` | Scalar variant-parameter JSON path | The same parameter JSON path |
| `{case_id}`, `{variant}`, `{repeat}` | Literal cell values | Literal cell values |
| `{file:PATH}` | Frozen declared file | Frozen declared file |

The case JSON contains `case_id`, `input`, `expected`, `fixtures`, `repeat`,
`params`, and `base_id`. A case's `fixtures` mapping is label → repository file
in the suite specification and label → relative local path in the wrapper input.
Generation writes a regular prediction file; its content can be JSON, text, or
another format understood by the supplied evaluator. An optional variant
`output_format` such as `json`, `txt`, `sdf` or `pdb` is preserved on prediction
artifacts; unspecified prediction formats are not guessed. Input/metrics artifacts
declare `json`, and fixture snapshots retain their original filename extension
as format metadata. A declaration does not establish that bytes parse correctly;
artifact viewers must verify the content before rendering. The evaluator writes:

```json
{"metrics": {"error": 0.25, "cost": 1.2}}
```

Every required metric must be a finite numeric value. Booleans, NaN, infinity,
missing output files, missing required metrics and unsuccessful process exits
produce failed rows. Raw stdout/stderr are diagnostic prefixes capped at 16 KiB
each, not metrics. Each process has a timeout; its process group is terminated
on interruption or timeout. Mutating case inputs, predictions during evaluation,
declared tool files, or fixtures invalidates the result. This detects drift; it
does not constrain a trusted local program's other filesystem or network access.

Use wrappers for the real tools you want to compare, with versioned code and
explicit fixtures. The included arithmetic wrappers demonstrate execution and
control plumbing; their numbers are not model-performance evidence. There is no
built-in paid-provider invocation, GPU workload, or automatic scheduler launch.

## Re-evaluate existing outputs

```bash
oms study rescore --results .oms/studies/results/benchmark.json \
  --evaluator docs/examples/study/evaluator.json --id evaluator-v2
```

Each pass receives a new `evaluation_pass_id` and new result-row IDs, with
`source_run_id` retaining the source identity. It stores the original result-file
byte digest, input and prediction byte digests, evaluator executable and declared
file digests, and the frozen evaluator manifest in its attempt record. The source
result and artifacts are checked before any evaluation begins. Source result
bytes are retained as an immutable object.

Only an evaluator runs. A generation command is neither reconstructed nor
invoked. Rows with predictions can be evaluated again even if their earlier
evaluation failed; rows without predictions are ineligible. Exactly one hashed
`input` artifact and one hashed `prediction` artifact are required per eligible
row. `--directions FILE` declares a new set of required metrics when the evaluator
changes the metric contract. The old pass is never overwritten.

SIGINT/SIGTERM during evaluation leaves a terminal `interrupted` result and
retains unexecuted source IDs. A hard process kill can leave an immutable attempt
without a terminal result; it is not treated as completed work. Create a new pass
to retry. There is no silent generation fallback or resume of a half-written pass.

## Corrections and reusable regression suites

```bash
oms study annotation add --results .oms/studies/results/benchmark.json \
  --run RUN_ID --expected corrected-expected.json \
  --rationale 'Reviewer corrected the expected value' --id correction-1
oms study annotation history --run RUN_ID --pretty
oms study annotation add --results .oms/studies/results/benchmark.json \
  --run RUN_ID --expected reviewed-expected.json --supersedes correction-1 \
  --rationale 'Second review resolved an ambiguity' --id correction-2
oms study annotation export --suite arithmetic --annotation correction-2 \
  --id regression-cases
oms study suite run --suite regression-cases --id corrected-benchmark
```

Each correction binds its exact suite, input, prediction and previous annotation
digests. A correction of a correction must explicitly name the current head;
concurrent conflicting updates are rejected under the shared state lock. History
is checked as a contiguous chain. Corrections never replace original expected
outputs or result rows.

Export creates a new immutable suite containing only the selected cases, with
their corrected expected outputs and original variant/evaluator snapshots. All
selected annotations must refer to the exact source suite. Choose one annotation
per case; historical corrections remain selectable by their explicit ID. Export
records those chosen IDs and hashes and assigns a new case-base digest. The new
suite can be executed as a regression benchmark with the normal run command.

## Pareto selection and optional adaptive search

```bash
oms study select pareto --results .oms/studies/results/benchmark.json \
  --constraints docs/examples/study/constraints.json --pretty
```

This computes the nondominated set of individual completed result rows. A row
dominates another when it is at least as good on every requested objective and
strictly better on one. `--objectives FILE` supplies metric → `min`/`max`; otherwise
the results' directions apply. Constraints are metric → `{min, max}` numeric
bounds. Incomplete rows, missing objective/constraint metrics and violations
are returned as exclusions. Equally scoring nondominated rows are retained.

Pareto selection is not search, does not aggregate across cases, and imports no
optional package. Supply a scientifically comparable set of runs; a mathematical
front does not make different cases or evaluation protocols interchangeable.

```bash
oms study select propose --results .oms/studies/results/benchmark.json \
  --space docs/examples/study/space.json --count 2 --seed 7 --id proposal-1
```

This separate action uses an optional installed Optuna dependency. It reconstructs
an in-memory multiobjective TPE study from completed observations and asks for
new parameters. Each observation must contain exactly the declared search-space
parameters. Numeric float/int ranges support `low`, `high`, `step` and `log`;
categorical distributions use scalar `choices`. Duplicate existing or newly
proposed configurations are not returned. If a finite space or the bounded
duplicate-attempt budget is exhausted, the result reports the shortfall.

The saved proposal binds source results, objective order, search space, seed and
Optuna version. It does not execute proposals, mutate the suite, install Optuna,
retain an optimizer service, or enforce outcome constraints on unseen proposals.
To continue adaptation, supply results that include the new measured observations.
`select pareto` remains usable without Optuna. The adapter uses Optuna's documented
[Study ask/add_trial interfaces](https://optuna.readthedocs.io/en/stable/reference/generated/optuna.study.Study.html)
and [TPE sampler](https://optuna.readthedocs.io/en/stable/reference/samplers/generated/optuna.samplers.TPESampler.html).

## Named immutable baselines

```bash
oms study baseline bind --name release-reference \
  --results .oms/studies/results/benchmark.json --run RUN_ID \
  --rationale 'Reviewed reference before the next change'
oms study baseline show --name release-reference --pretty
oms study baseline list
```

A name binds once to the selected successful runs, result source bytes, artifact
bytes, metric directions, control digests, and rationale. Omitting `--run` selects
all completed rows; repeat the flag for an explicit subset. Rebinding an existing
name is refused even if the new target looks similar. Create another name for a
new baseline. `show` verifies the retained source and artifact objects, so editing
the original result file does not move the binding. Structural artifact metadata
such as `frame` and `alignment` is retained without inferring alignment quality.

## Stage-based what-if forks through DVC

The supported adapter is **DVC, one root JSON-encoded `dvc.yaml`, regular-file
`cmd`/`deps`/`outs` stages**. JSON is a YAML subset; the restriction permits exact
stdlib inspection of the actual pipeline. General YAML syntax, `params`,
`foreach`, `matrix`, `wdir`, interpolation, remote/directory outputs, output
options, and stage/output ownership expansion are rejected. Track a parameter
JSON file as a file dependency to change it within this scope. Capture a new base
for an expanded topology. Nextflow and Snakemake adapters are not implemented.

An example compatible pipeline is:

```json
{
  "stages": {
    "prepare": {"cmd": "python3 prepare.py source.json prepared.json", "deps": ["prepare.py", "source.json"], "outs": ["prepared.json"]},
    "score": {"cmd": "python3 score.py prepared.json params.json scored.json", "deps": ["score.py", "prepared.json", "params.json"], "outs": ["scored.json"]}
  }
}
```

Include every script and input read by a stage in its dependencies. The adapter
derives edges from actual output → dependency relationships, validates cycles,
and preserves file executable bits. The DVC stage command itself retains DVC's
shell-command semantics; it is trusted user code.

Create `dvc-adapter.json` as `{"argv":["dvc"],"timeout_seconds":300}` and
`replacements.json` as `{"params.json":"alternatives/params-v2.json"}`:

```bash
oms study workflow capture --id prepared-base --adapter dvc-adapter.json
oms study workflow plan --base prepared-base --replacements replacements.json --pretty
oms study workflow fork --base prepared-base --replacements replacements.json \
  --expected-plan-sha256 REVIEWED_PLAN_SHA256 --id alternative
```

Capture requires all declared outputs to exist and `dvc status --json
--no-updates` over the declared stage targets to report no changes. It snapshots
the pipeline, lock file when present, external input
files and completed outputs, then checks that the source bytes did not change
during the status probe. It does not generate the base. `--cache-dir .dvc/cache`
can explicitly bind an existing local DVC cache for subsequent fork workspaces.

The read-only plan lists directly changed stages, affected descendants, reusable
stages and exact reused-output digests. A supplied plan digest detects replacement
drift between review and execution. Fork creates a fresh directory, restores the
captured bytes, applies only explicit replacement files, invokes `dvc init
--no-scm`, optionally configures the chosen native DVC cache, then calls `dvc repro
--single-item dvc.yaml:STAGE` for each affected stage in topological order.

Unchanged stages are not requested. Their outputs are verified after the run;
unexpected mutation or disappearance makes the fork fail. Pipeline/source input
drift is also recorded as failure. The report contains all bounded adapter
receipts, actual changed-output paths and new output digests. Requested downstream
stages may be skipped or restored by DVC's own rules; unchanged output bytes do
not prove a cache hit. `native_cache_hits` therefore remains `unknown`. There is
no OMS workflow cache or scheduler. The working directory is isolated, but stage
commands are not sandboxed.

The command and stage semantics are grounded in the official
[DVC repro reference](https://doc.dvc.org/command-reference/repro),
[DVC status reference](https://doc.dvc.org/command-reference/status), and
[dvc.yaml specification](https://doc.dvc.org/user-guide/project-structure/dvcyaml-files).
`--single-item` turns off recursive dependency reproduction, which is why the
adapter explicitly orders the affected stages and verifies reused outputs.

## Result interface and durability

[The JSON schema](examples/study/results.schema.json) describes the shared report
and artifact-view input:

```json
{
  "schema": 1,
  "kind": "oms-study-results",
  "metric_directions": {"error": "min"},
  "rows": [{
    "run_id": "run-123", "case_id": "case-1", "variant": "baseline", "repeat": 0,
    "status": "complete", "metrics": {"error": 0.25}, "params": {"temperature": 0},
    "artifacts": [{"path": "outputs/prediction.json", "type": "prediction", "label": "Prediction"}]
  }]
}
```

Row IDs are unique. Complete rows contain all declared required metrics; failed
and interrupted rows may have empty metrics. Artifact paths are normalized,
repository-relative, non-symlink paths. `sha256` is optional for read-only numeric
analysis and required for rescore, corrections and artifact-bound baselines.
Additional artifact metadata, including structural frame/alignment declarations,
is preserved. The format contains no new candidate identity field.

New state lives under `.oms/studies`: immutable named JSON records, SHA-256
content objects, and per-attempt working directories. Writes reuse the runtime's
bounded JSON, atomic writer, private directories and existing file-lock protocol.
Every stored record includes a digest checked on read. Filesystem owners can
still alter bytes; such modifications are detected rather than silently accepted.
Records and objects are local research data and may contain private fixtures or
diagnostics. They are not sanitized portable capsules.

Limits are 8 MiB per JSON record, 32 MiB per artifact, 10,000 planned/result cells,
1,000 cases, 100 variants, and 50 directed metrics. Directory artifacts and large
model checkpoints belong in a dedicated artifact system. No object garbage
collection or automatic retention pruning is provided. Normal execution errors
return exit 1 with result JSON; input/contract errors return exit 2; an interrupted
execution returns 130. An interrupted command never counts as complete coverage.

Marked harness children may inspect results, coverage, annotation history,
baselines, pure Pareto selection and workflow plans. Mutating and executing study
actions fail closed before writing state. The standalone command uses the same
`OMS_HARNESS_CHILD=1` or nonzero `OMS_HARNESS_DELEGATE_DEPTH` cooperative authority
signals as the runtime.

## Verification

`python3 tests/test_studies.py -v` exercises real local subprocess wrappers in
temporary repositories with isolated HOME and lock caches. It covers execution,
missing and nonfinite metrics, timeout/log bounds, terminated evaluation, artifact
tampering, duplicate coverage, corrections, baseline immutability, Pareto
constraints, and a fixture DVC CLI that executes the declared stages. Separate
optional tests use real installed Optuna and DVC; they are skipped when absent.
Fixture DVC tests verify our adapter
contract, not compatibility with every DVC release; validate the installed DVC
version against the supported scope before a substantial workflow run. The
integration tests have been exercised with DVC 3.67.1 and Optuna 5.0.0.
