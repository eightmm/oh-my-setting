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

## Splitting And Leakage (highest priority)

Random splits leak in chem-bio. Near-duplicate molecules/sequences land in
both train and test, so the metric reports memorization, not generalization.

- **Molecules**: scaffold split (Bemis-Murcko) or cluster split by fingerprint
  similarity. Never random-split a congeneric series.
- **Proteins/sequences**: split by sequence identity (e.g. MMseqs2/CD-HIT
  cluster at a stated threshold), not by random record. State the threshold.
- **Structures/complexes**: split by sequence/fold AND by ligand scaffold when
  both vary; time-split for docking benchmarks where applicable.
- **Targets/assays**: keep all measurements of one target (or one assay) on a
  single side of the split when the task generalizes across targets.
- Fit scalers, feature selection, thresholds, and imputation on train only.
- Deduplicate (canonical SMILES / sequence hash) BEFORE splitting.

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

## Featurization Conventions

- **RDKit**: canonicalize SMILES; sanitize and skip/triage molecules that fail
  parsing rather than dropping silently; fix a fingerprint radius/size and
  record it; standardize tautomer/charge/salt with a stated protocol.
- **Sequences**: record the embedding source and version (e.g. ESM variant);
  pin max length and truncation policy; mask non-standard residues explicitly.
- **3D/structure**: record coordinate source (crystal vs predicted), protonation
  and conformer protocol; keep equivariance assumptions explicit.
- Cache features keyed by (input hash + featurizer version); invalidate on bump.

## Reproducibility

- Record per run: data version/split file hash, featurizer version, seed,
  config, commit, checkpoint — via the run ledger.
- A result without its split file is not reproducible; commit or hash the split.
- Re-run the cheap baseline whenever the split changes.

## Pre-Training Stop

Before launching training or a Slurm job, confirm: dedup done, split policy
written and domain-appropriate, label units/direction verified, metric chosen
for the imbalance, baseline defined. Run the ML review gate
(`multi-agent-review --ml`) when the change touches data, split, loss, or metric.
For experiment design and what a result means, load `research-method`; this
skill only governs domain correctness.
