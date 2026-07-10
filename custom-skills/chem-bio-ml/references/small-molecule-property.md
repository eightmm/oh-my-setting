# Small-Molecule Property And Activity

Use for QSAR, ADMET, toxicity, physicochemical properties, bioactivity, HTS,
fingerprints, SMILES/SELFIES models, and molecular graphs.

## Intake

- Parse and sanitize explicitly. Quarantine failures with reason codes; report
  coverage instead of silently dropping molecules.
- Apply a documented `rdMolStandardize` pipeline before deduplication: cleanup,
  fragment/salt policy, charge policy, tautomer policy, isotope policy, and
  stereo policy. Preserve the raw structure and mapping.
- Do not assume RDKit assigns biologically relevant protonation. State the pH
  and external protonation method when charge state matters.
- Hash the standardized representation used for splitting. Keep stereoisomers
  distinct when the endpoint can distinguish them.

## Representation

- Record descriptor definitions and software version. Fail on NaN/inf; do not
  convert invalid descriptors to zeros.
- Fix fingerprint type, radius, length, chirality, and feature flags. Include a
  fingerprint + RF/GBM baseline.
- For SMILES/SELFIES, define canonical/random enumeration policy, tokenizer,
  vocabulary fit boundary, maximum length, and invalid-decoding behavior.
- For graphs, preserve formal charge, aromaticity, isotope, chiral tags, and
  bond stereo. Assert an R/S pair differs after featurization.
- In PyG-style batches, pool and normalize by graph membership. Assert a
  molecule's output is unchanged by unrelated batch members.

## Split And Labels

- Match the deployment question: random only for true interpolation; otherwise
  use scaffold, similarity-cluster, matched-series, assay/campaign, source, or
  temporal splits.
- Keep exact structures, standardized parents, close analogs, and replicate
  measurements together. Report train-test nearest-neighbor similarity.
- For pooled assays, retain endpoint, target construct, organism, cell context,
  pH, temperature, units, and qualifiers. Do not average incompatible assays.
- Verify transforms such as IC50 to pIC50 and ΔG sign. Persist transform state
  and report errors in original units.
- For preclinical PK, preserve species/strain, route, dose, formulation,
  sampling schedule, compartment, and endpoint. Do not merge clearance, AUC,
  half-life, bioavailability, or time points as interchangeable labels; group
  measurements from one animal/study/campaign during splitting.
- Detect activity cliffs and report them separately; global RMSE can hide poor
  ranking within a medicinal-chemistry series.

## Metrics And Baselines

- Regression: MAE/RMSE plus Pearson and Spearman; add within-series or
  within-target ranking when decisions are rank-based.
- Imbalanced classification: PR-AUC, enrichment/EF, BEDROC, and thresholded
  precision/recall over ROC-AUC alone.
- Audit negatives with simple physicochemical-property models. Easy separation
  by MW/logP/charge signals a biased benchmark.
- Stratify by scaffold, similarity, source, assay, and applicability domain.

Evidence anchor: [MoleculeNet](https://pubs.rsc.org/en/content/articlelanding/2018/sc/c7sc02664a)
separates quantum, physical, biophysical, and physiological tasks and emphasizes
task-appropriate splits, metrics, and physics-aware features.
