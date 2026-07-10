# Biomolecular Interactions And Complexes

Use for DTI, protein-ligand docking and affinity, virtual screening, PPI,
protein-peptide, protein-nucleic-acid, and interface or pose prediction.

## Define The Task

- Distinguish binding classification, affinity regression/ranking, pose
  generation, pose scoring, pocket detection, and interaction-site prediction.
  A docking score is not an experimental affinity label.
- Name all entities and contexts: ligand parent/stereo/protonation, protein
  construct/state, pocket, cofactors, assay, species, and experimental method.
- For complexes, record apo/holo status, biological assembly, bound ligand,
  template origin, and whether the inference workflow actually has that pose or
  pocket.

## Split Matrix

- Report warm pair, cold-drug, cold-target, and cold-both results separately.
  Do not label a random pair split as novel-target generalization.
- Cluster drugs by scaffold/similarity and proteins by sequence/family/fold.
  Dual-cold evaluation holds out both cluster axes.
- For PPI or other networks, distinguish edge holdout from inductive node or
  family holdout. Shared hubs make edge-random splits easy.
- Keep alternate poses, structures, mutants, assay replicates, and close
  homolog/analog complexes together. Audit template and structural-database
  leakage.
- Use temporal or campaign holdouts for prospective docking/screening claims.

## Negatives And Decoys

- Treat missing edges as unknown, not negative. Record measured-inactive,
  non-binding, decoy, random, or sampled-unobserved provenance.
- Match nuisance properties without making negatives trivially separable by
  size, charge, sequence length, degree, compartment, or assay source.
- For docking, separate generated incorrect poses from non-binders. They test
  different decisions.
- Re-sample training negatives without contaminating validation/test and keep
  the evaluation negative set frozen.

## Model And Feature Guards

- Prevent ligand-frame leakage and bound-pose access when screening from apo
  structures or sequence. Define pocket construction without test ligand hints.
- Align residues/atoms across sequence, structure, and interaction labels.
- Preserve interaction symmetry where appropriate; order-sensitive models for
  symmetric pairs require swap-invariance tests.
- Verify graph/global attention and pooling isolate complexes within a batch.

## Evaluation

- Screening: PR-AUC, EF@fixed fraction, BEDROC, hit rate, and property-only
  decoy audit; global ROC-AUC is insufficient.
- Affinity: MAE/RMSE plus Pearson/Spearman, within-target ranking, and cold-axis
  slices in original units.
- Pose: top-k success at a stated RMSD/interface criterion, stratified by pocket
  flexibility, ligand similarity, and apo/holo source.
- PPI/interface: relation-aware PR metrics and per-protein/interface results;
  report degree and family slices.
- Baselines: ligand-only, protein-only, nearest known pair, docking score, and
  simple concatenation. A pair model must beat single-entity shortcuts.

Evidence anchor: [DrugOOD](https://ojs.aaai.org/index.php/AAAI/article/view/25970)
demonstrates large affinity-prediction gaps under realistic biochemical OOD
domains and noisy annotations.
