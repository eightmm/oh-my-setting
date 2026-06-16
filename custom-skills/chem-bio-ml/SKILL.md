---
name: chem-bio-ml
description: >
  Chem-bio ML guardrails: molecule/protein data splitting and leakage,
  domain metrics and their traps, featurization conventions (RDKit, sequence,
  structure), and label semantics. Use when writing or reviewing models on
  molecular, protein, or other chem-bio data — especially before training or
  a pre-training review gate.
---

Domain guardrails loaded on demand. The silent-failure modes here cost GPU
days and retracted results, not crashes — check them before training.

## Intake Standardization (run BEFORE dedup and split)

The same compound enters as multiple distinct objects — salt forms, tautomers,
protonation/charge states, stereo. Dedup/split on the raw input and near
duplicates leak across train/test even with a correct scaffold split, and
featurization runs on inconsistent molecules. Standardize first, then dedup and
split on the standardized form; hash that form; log every normalization choice.

- **Molecules (RDKit `rdMolStandardize`)**: Cleanup → FragmentParent /
  LargestFragment (strip counterions/salts) → Uncharger → canonical tautomer
  (`TautomerEnumerator`). Decide stereo explicitly (keep vs flatten). RDKit does
  NOT assign pKa/protonation — if 3D charge matters, protonate at a stated pH
  with an external tool (dimorphite-DL / OpenBabel) and record the tool + pH.
- **Proteins/structures (PDB cleaning)**: select chain(s); pick one alt-loc
  (e.g. A); strip waters/HETATM unless the ligand; flag/fill missing residues;
  map non-standard residues. Record the protocol.
- Over-normalization is its own bug: collapsing stereoisomers/tautomers the
  target distinguishes destroys signal. Make each choice explicit, not silent.

```python
from rdkit.Chem.MolStandardize import rdMolStandardize
def standardize(mol):
    mol = rdMolStandardize.Cleanup(mol)
    mol = rdMolStandardize.FragmentParent(mol)          # strip salts/counterions
    mol = rdMolStandardize.Uncharger().uncharge(mol)
    mol = rdMolStandardize.TautomerEnumerator().Canonicalize(mol)
    return mol  # canonicalize SMILES + hash THIS for dedup/split keys
```

## Splitting And Leakage (highest priority)

Random splits leak in chem-bio. Near-duplicate molecules/sequences land in
both train and test, so the metric reports memorization, not generalization.

- **Molecules**: scaffold split (Bemis-Murcko) or cluster split by fingerprint
  similarity. Never random-split a congeneric series.
- **Proteins/sequences**: split by sequence identity (e.g. MMseqs2/CD-HIT
  cluster at a stated threshold), not by random record. State the threshold.
- **Structures/complexes**: split by sequence/fold AND by ligand scaffold when
  both vary; time-split for docking benchmarks where applicable. **Template
  leakage**: for structure-based/docking models, record whether the bound
  ligand or a close analog appeared in the training set OR the template/
  homology database — a test "win" may just be a seen complex.
- **Targets/assays**: keep all measurements of one target (or one assay) on a
  single side of the split when the task generalizes across targets.
- Fit scalers, feature selection, thresholds, and imputation on train only.
- Deduplicate (canonical SMILES / sequence hash) BEFORE splitting.

Give these guardrails teeth: fingerprint the splits and assert no ID overlap
before training, and detect silent split drift between runs. Use a cluster/
scaffold/target ID as the key (the unit you split on), not the row.

```bash
~/.oh-my-setting/scripts/data-manifest.sh create --name pl-v1 --id-column cluster_id \
  --split train=splits/train.csv --split val=splits/val.csv --split test=splits/test.csv
~/.oh-my-setting/scripts/data-manifest.sh leakage --name pl-v1   # nonzero if any split shares an ID
~/.oh-my-setting/scripts/data-manifest.sh check   --name pl-v1   # nonzero if a split's ID set drifted
```

If the split policy is not written in PROJECT.md, ask before training.

## Labels And Targets

- Confirm units and direction before modeling: IC50 vs pIC50, Ki vs Kd,
  ΔG sign, % vs fraction. A flipped sign or unlogged target silently wrecks
  the loss landscape.
- Censored/qualified values (`>`, `<`) are not point labels — decide drop vs
  censored-loss explicitly.
- Aggregate replicate measurements deliberately (median vs mean vs keep-all);
  document it.
- Class imbalance is the norm (actives ≪ inactives); pick metric and sampling
  accordingly.

## Public Multi-Source Data (ChEMBL/PubChem/BindingDB)

Scraping and merging public records across assays/labs/formats is itself the
main silent-failure source — the data looks unified but isn't comparable.

- Keep per-measurement provenance: `assay_id`, organism, target accession/
  isoform, mutation/construct, readout type, endpoint, units, pH/temp, cell
  line. Missing fields → mark `unknown`, never assume a default.
- **Conflicting labels**: the same molecule–target pair across assays often
  disagrees by >1 log unit. Detect duplicates by (standardized molecule, target)
  and decide drop / aggregate / keep-by-assay explicitly — never silently mean
  incomparable readouts.
- **Negative provenance**: never treat an unmeasured compound as inactive. Tag
  `negative_source` (measured-inactive / assumed / decoy / random) and report
  metrics for measured inactives and decoys separately (see Negative Sets).
- **Leave-assay/campaign-out**: the real generalization test for pooled data is
  holding out a whole assay or campaign, not just scaffolds. The same molecule
  from one assay family on both sides of a scaffold split is still leakage.

## Metrics And Their Traps

- Report metrics on the domain split, never a random split.
- Regression: alongside RMSE/MAE report Pearson AND Spearman; a high global R²
  can hide useless within-series ranking.
- Classification on imbalanced data: PRC-AUC / enrichment / BEDROC over plain
  ROC-AUC; state the active threshold.
- Virtual screening: early-enrichment (EF@1%, BEDROC) is the decision metric,
  not global AUC.
- Always compare against a cheap baseline (fingerprint + RF/GBM, or sequence
  identity nearest-neighbor). A deep model that loses to it is not working.
- **Report in original units.** Fit target scalers / log-transforms on train
  only (see Splitting), persist them with the checkpoint, and apply — never
  refit — at val/test/inference. Compute RMSE/MAE/R² AFTER inverse-transform;
  error in scaled or log space is not the error you think it is. Assert the
  inverse round-trips, and that pIC50↔IC50 direction matches the label.

## Negative Sets And Activity Cliffs

The benchmark is only as honest as its negatives and its hard cases.

- **Decoy/negative bias**: DUD-E-style decoys separate on trivial physchem
  (MW, logP, charge), so a model scores high AUC while learning nothing about
  binding. Audit: a property-only logistic baseline must FAIL to separate
  classes — if it succeeds, the benchmark is broken. Prefer experimentally
  confirmed inactives; record how negatives were chosen.
- **Activity cliffs**: near-identical structures with large ΔpActivity violate
  the smoothness models assume and dominate error. Similarity splits scatter
  cliff pairs unpredictably across train/test. Detect cliff pairs (high
  similarity + large activity gap) and report their performance separately.

## Applicability Domain And Calibration

Retrospective split metrics say nothing about a NEW prospective compound. Models
extrapolate confidently off the training manifold — exactly when researchers
pick compounds to synthesize. Report which predictions are trustworthy.

- **AD flag**: distance to k-nearest training neighbors (Tanimoto for FPs,
  Euclidean/KDE for embeddings); threshold is dataset-specific — report the
  number, not a magic cutoff. Stratify metrics in-AD vs out-of-AD.
- **Uncertainty**: ensemble or MC-dropout variance (epistemic); conformal
  prediction for distribution-free intervals.
- **Calibration**: for regression, check predicted-vs-empirical interval
  coverage; for classification, a reliability curve. Confident + wrong off-AD is
  the failure that wastes wet-lab cycles.

## Featurization Conventions

Common to all modalities: cache features keyed by (input hash + featurizer
version) and invalidate on bump. Use ONE canonical `featurize()` shared by train
and inference — a second code path is how offline val looks great and deployment
is garbage. Pin and record RDKit/ESM versions (descriptor/fingerprint
definitions drift between versions) and seed conformer generation. Fail-fast on
NaN/inf features (assert, never zero-fill — a silently zeroed descriptor is a
wrong feature, not a missing one). Modality-specific guards below.

### Molecule

- **RDKit**: canonicalize SMILES; sanitize and skip/triage molecules that fail
  parsing rather than dropping silently; fix a fingerprint radius/size and
  record it; standardize tautomer/charge/salt with a stated protocol.
- **Chirality/stereo**: many mol→graph converters drop chiral tags and bond
  stereo, forcing enantiomers with different activity onto identical graphs.
  Assert a featurized R/S pair produces distinct representations.
- **Graph batching isolation**: PyG collates molecules into one disjoint
  graph. Any global op (global pooling, batch-level norm, global attention) that
  aggregates across the flat node tensor without slicing by the `batch` index
  silently leaks information between molecules in a batch. Unit-test: a
  molecule's output must be identical alone vs. inside a batch.

### Sequence

- **pLM embeddings**: record source + version (e.g. ESM variant); pin max length
  and truncation policy; mask non-standard residues explicitly. Mean-pool with
  the attention mask and EXCLUDE special tokens (BOS/EOS/CLS) — pooling over
  padding or specials corrupts every vector. Assert truncation
  (`len(seq) <= max_len`, else chunk/flag), run under `eval()`/`no_grad`, and
  verify the embedding is invariant to batch composition (same seq → same vector
  regardless of padding neighbors).

### Structure

- **3D coordinates**: record coordinate source (crystal vs predicted),
  protonation and conformer protocol; keep equivariance assumptions explicit.
  Train-on-crystal / screen-on-RDKit-conformer is a silent OOD shift — if
  evaluating on generated conformers, report metric variance across an
  ETKDG+MMFF ensemble.
- **Coordinate frame & augmentation (data pipeline)**: center coordinates (COM)
  and apply per-sample random rotations during training, rotating any vector/
  force targets consistently; assert a single unit convention (Å vs nm). Watch
  for **ligand-frame leakage** — centering/orienting the pocket by the bound
  ligand lets the model find the site at the origin, so it collapses on apo or
  screening inputs. This guards the data frame; the SE(3) test in `ml-training`
  guards the architecture — you need both.
- **pLM↔structure residue alignment** (sequence+structure fusion): PDB files
  have missing residues, insertion codes (`82A`), and non-sequential numbering.
  Matching the i-th FASTA char to the i-th ATOM record silently pairs embeddings
  with the wrong 3D coordinates and trains on garbage without crashing. Align
  FASTA↔PDB sequence (Biopython pairwise), assert high identity, and verify zero
  misalignment around missing-density regions before fusing the two.

## Reproducibility

- Record per run: data version/split file hash, featurizer version, seed,
  config, commit, checkpoint — via the run ledger.
- A result without its split file is not reproducible; commit or hash the split.
- Re-run the cheap baseline whenever the split changes.

## Pre-Training Stop

Before launching training or a Slurm job, confirm: intake standardization done
and hashed, dedup done, split policy written and domain-appropriate, label
units/direction verified, multi-source labels harmonized (assay conflicts
resolved, negative_source tagged), negatives audited (property-only baseline
fails), metric chosen for the imbalance, baseline defined, AD/uncertainty
plan stated.
Run the ML review gate
(`multi-agent-review.sh --ml`) when the change touches data, split, loss, or metric.
For experiment design and what a result means, load `research-method`; this
skill only governs domain correctness.
