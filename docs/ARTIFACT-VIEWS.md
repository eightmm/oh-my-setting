# Offline artifact views

`oms artifact-view` turns recorded experiment results into one self-contained
HTML file. Open it locally to filter cases, compare numeric parameters and
metrics, inspect retained outputs, record judgments, and write a research
decision. It does not start a web server or request remote resources.

```bash
oms artifact-view --repo . build --input results.json
oms artifact-view --repo . capsules capsule-a capsule-b --title 'Docking comparison'
oms artifact-view --repo . list
```

The JSON response identifies the HTML path, its SHA-256, the source digest, and
stable row keys. The default output is private `.oms/artifact-views/`; use
`--output reports/comparison.html` for an explicit repository-relative location.
An existing output requires `--replace`. Capsule imports label each capsule as
an **aggregate** case; they do not invent per-molecule observations.

## Results contract

A workflow can emit the following results format:

```json
{
  "schema": 1,
  "kind": "oms-study-results",
  "metric_directions": {"rmsd": "min", "success_rate": "max"},
  "rows": [
    {
      "run_id": "trial-01",
      "case_id": "complex-001",
      "variant": "candidate",
      "repeat": 0,
      "status": "complete",
      "metrics": {"rmsd": 1.4, "success_rate": 1.0},
      "params": {"seed": 17, "temperature": 0.2},
      "artifacts": [
        {
          "path": "outputs/pose.sdf",
          "type": "molecule",
          "label": "Retained pose",
          "frame": "receptor input coordinates",
          "alignment": "none applied"
        }
      ]
    }
  ]
}
```

The tuple `(run_id, case_id, variant, repeat)` must be unique. Repeats are
nonnegative integers no greater than JavaScript's exact integer limit, 2^53−1.
Metrics are finite
numbers or null; parameters are scalar values. Supported top-level provenance
fields are `suite_id`, `base_id`, `evaluator_sha256`, `evaluation_pass_id`, and
`source_sha256`. The viewer preserves these values in the source digest.

For group summaries only complete rows with the selected metric are included.
The table shows every matching status. Baseline deltas pair by exact case and
repeat in the selected baseline **variant or run**; missing or ambiguous baseline matches
produce no delta. A delta is row minus baseline; color uses the explicitly
provided metric direction. This is descriptive comparison, not a statistical
significance test or a claim that different protocols/units are comparable.

Search matches case, run, variant and parameter text. The scatter plot accepts
a numeric parameter or another metric on X. The table is paginated at 100 rows;
the plot explicitly labels a 1,500-point cap. Filters, group summaries and report
exports retain all matching records, up to the 10,000-row input limit.
Axis positions and tick labels remain finite even when the distance between
two finite extrema would overflow. A constant-valued axis places its points at
the center; nonfinite X values are excluded from the scatter plot.

## Artifact inspection and molecular comparison

Text (`txt`, `json`, `csv`, `tsv`, `md`), PNG/JPEG/GIF/WebP images and molecular
PDB/SDF/MOL2/XYZ files can be embedded. An explicit artifact `format` (such as
`json` or `sdf`) supports content-addressed paths with no extension; otherwise
the filename extension is used. Undeclared prediction formats are not guessed. Other formats remain labeled references.
Paths must be relative to the selected repository; symlinks and traversal are
refused. A supplied artifact `sha256` must match its bytes. The view records
the computed digest even when the source did not supply one.

To enable molecular rendering, pass a local copy of
[3Dmol.js](https://3dmol.org/doc/index.html):

```bash
oms artifact-view --repo . build --input results.json \
  --molecule-js vendor/3Dmol-min.js --output reports/poses.html
```

The caller-supplied JavaScript is executable code and should be a reviewed,
pinned vendor asset. Its digest is recorded. OMS does not download it implicitly
or vendor a moving CDN build. With it embedded, the two WebGL viewers share
camera orientation and work offline. Rendering needs a browser with WebGL.
Without the asset, molecular coordinates remain available as text.

The frame and alignment strings are supplied evidence. Camera synchronization
does not align atom coordinates, validate chemical structures, calculate RMSD,
or map atoms between ligands. Use your scientific workflow to perform those
operations and record their provenance before comparing aligned poses.

## Decisions, feedback, and durable imports

Open a case or click a scatter point to inspect it. Select cases, record accept /
reject / uncertain judgments or comments, then export feedback. Write the
question, decision and next experiment in the report section.

- **Save this view** downloads another standalone HTML file retaining filters,
  selection, annotations, report fields and structure choices.
- **Export Markdown report** includes source digest, filter settings, all
  matching metric rows and selected/annotated case identities.
- **Export all feedback** downloads schema-1 `oms-artifact-feedback` JSON for
  every annotated case and every checked case without an annotation.
- **Export selected feedback** downloads only the checked cases, including
  their full judgments and comments. It does not include unchecked annotations.
  Both exports retain the same source identity and current report fields.

To export a large review in parts, use **Clear selection**, check a batch of
cases (or use **Select this page**), and choose **Export selected feedback**.
Clear the selection before choosing the next batch. Selection can span filters
and pages; the selected count includes all checked cases. Clearing selection or
exporting a batch does not remove any annotation. Import each downloaded file
explicitly to make those browser choices durable OMS records:

```bash
oms artifact-view --repo . import-events view-feedback.json
oms artifact-view --repo . events --view view-SOURCE_DIGEST_PREFIX
```

Imports must identify a view registered in this repository and its exact source
digest. Unknown row keys, unsupported actions and credential-like text are
refused. Re-importing the same batch is idempotent. Different batches append to
history rather than replacing a prior judgment. Imported `report.next` remains
recorded text; it is not executed as a command or silently sent to a model.
The optional `oms input feedback` adapter can queue it for explicit delivery.

Browser state is not written back automatically. Feedback supports up to 1,000
events and 1 MiB of serialized UTF-8 per file; comments are bounded at 4,000
characters. An export exceeding either bound is refused before download and
keeps every comment in the view. Choose a smaller selection and export it as
another batch. Comments are never shortened to fit a batch. Saved views preserve
annotations for all source cases, including reviews with more than 1,000
annotations, subject to the 24 MiB standalone HTML limit; oversized saved HTML
is refused rather than truncated. Original
record inputs are limited to 8 MiB, each embedded artifact to 2 MiB, embedded
artifact bytes to 12 MiB, and the final HTML to 24 MiB.

## Remote result snapshots

```bash
oms artifact-view --repo . remote --host cluster-login \
  --remote-root /srv/project --input results/comparison.json \
  --output reports/cluster-snapshot.html
```

This reads a results file and its supported artifact paths with SSH batch mode
and a bounded Python 3 reader on that host. File paths are interpreted relative
to the explicit remote root; symlink descendants are refused. Remote files are
limited to 2 MiB each. The SSH identity/configuration must already work; OMS
does not enroll hosts, upload files, submit jobs, or expose a network port.

The result is a read-only **snapshot**, not a live remote workspace or tunnel.
Repeat the explicit command with `--replace` to refresh it. Data hashes identify
the bytes actually captured. For mutually consistent multi-file snapshots,
publish an immutable results directory upstream and supply artifact hashes.

## Verification

`python3 tests/test_artifact_view.py` exercises the new command family end to end:
offline payload encoding, artifact digest/path checks, source-bound idempotent
feedback, child write refusal, capsule adaptation, and SSH command quoting /
symlink refusal using a local fixture executable. Optional Node checks exercise
the same pure functions used by the UI for finite scatter axes, saved annotation
restoration, selected feedback batches and the UTF-8 size bound; a generated
selected batch is also imported by the real Python command. `node --check
scripts/lib/artifact_view_assets/view.js` checks browser-script syntax. No
browser engine, GPU library, paid model or live SSH host is required by these
tests.
