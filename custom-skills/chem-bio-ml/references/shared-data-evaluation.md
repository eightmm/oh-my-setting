# Shared Data And Evaluation Contract

Apply this reference to every chem-bio task.

## Define The Scientific Unit

- Name the prediction unit: molecule, conformer, residue, sequence, complex,
  reaction, well, cell, donor, perturbation, or graph edge.
- List every entity that can recur across rows. Assign immutable standardized
  IDs before deduplication and splitting.
- State what is known at inference. Remove features derived from the label,
  future measurements, bound structures, downstream assays, or test databases.
- Define the deployment population. Choose split keys to model that population,
  not to maximize sample count or leaderboard comparability.

## Provenance And Harmonization

- Retain source, release/snapshot, assay/protocol, organism, construct/isoform,
  conditions, units, qualifiers, and processing version per observation.
- Standardize before deduplication. Log every destructive choice; preserve raw
  values and the mapping from raw to standardized IDs.
- Do not average measurements that differ in endpoint, protocol, biological
  context, or units. Model assay/source effects or keep them separate.
- Treat `<`, `>`, detection-limit, and interval labels as censored. Use a
  censored objective, interval metric, or an explicit exclusion policy.
- Keep replicate identity. Aggregate only after checking technical versus
  biological replicates and defining the estimand.
- Record `negative_source`. Unmeasured, corrupted, random, decoy, and measured
  inactive examples are not interchangeable negatives.

## Split And Leakage

- Split on the generalization unit, not the row. Consider entity, pair,
  scaffold/family, assay/campaign, donor/batch, source, and time together.
- Deduplicate exact and near duplicates before splitting. Keep all derived views
  of one entity or experiment on one side unless the task explicitly tests them.
- Build label-blind split groups (for example scaffold, sequence-family, or
  entity-similarity clusters) over the declared entity universe before assigning
  splits; freeze the algorithm, inputs, threshold, and cluster IDs.
- Fit learned or label-informed transforms on training data only: normalization,
  imputation, feature selection, vocabularies, representation-space clustering,
  retrieval indexes, thresholds, and calibration.
- For multimodal data, align views through immutable entity/experiment IDs,
  keep all views on one split side, record missingness and inference-time
  modality availability, and compare against single-modality ablations.
- Keep model-selection validation, calibration, and test roles distinct. Use
  validation for early stopping/model choice; after freezing the model, fit
  calibration on a disjoint reserved subset or use a nested/cross-fitted
  protocol. Open test only for the fully frozen model and calibrator.
- Hash split membership and preprocessing configuration. Fail on overlap or
  drift between runs.

## Evaluation

- Predeclare one primary metric tied to the decision. Report uncertainty over
  seeds, folds, campaigns, donors, or other independent experimental units.
- Report metrics in original scientific units after inverse transformation.
- Include a cheap domain baseline and a leakage-sensitive baseline. Complex
  models that do not beat nearest-neighbor, frequency, linear, or tree baselines
  have not demonstrated useful learning.
- Report group slices: source/assay, scaffold/family, target, donor/cell line,
  class prevalence, and in-domain versus out-of-domain.
- Measure calibration: reliability/ECE for classification; interval coverage
  and width for regression. Never reuse adaptively selected validation rows as
  calibration data; reserve them in advance or use nested/cross-fitting.
- Define an applicability-domain score from training neighbors or density.
  Report coverage and performance at the chosen threshold; do not hide rejected
  predictions.

## Required Assertions

- Train/eval IDs and declared group keys do not overlap.
- Transform round-trips preserve units and label direction.
- Featurization is deterministic at inference and invariant to batch neighbors.
- Missing/invalid inputs fail or are explicitly quarantined; never silently
  zero-fill scientifically meaningful features.
- One example produces the same prediction alone and in a mixed batch, within
  the declared numerical tolerance.
- The frozen checkpoint carries data, split, featurizer, scaler, and config IDs.

## Stop Checklist

Do not train until provenance, split policy, label semantics, negative policy,
baseline, primary metric, calibration/AD plan, and inference feature boundary
are written and mechanically checkable.
