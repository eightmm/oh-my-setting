# Protein Structure And Variant Effects

Use for experimental or predicted structures, pockets/interfaces, structural
quality, inverse folding, protein variants, DMS, and fitness prediction.

## Structure Intake

- Record PDB/model accession, release, experimental method/resolution, chain,
  assembly, construct, mutations, ligands/cofactors, and predicted confidence.
- Resolve alt-locs, insertion codes, non-standard residues, missing atoms and
  residues, chain breaks, waters, ions, and biological assembly explicitly.
- Align reference/FASTA sequence to coordinates. Never map the i-th sequence
  residue to the i-th ATOM record without insertion/missing-density handling.
- Distinguish apo/holo, monomer/assembly, experimental/predicted, and bound/
  generated conformations. These are different data distributions.

## Structure Splits And Features

- Split by sequence cluster plus fold/domain or interface family when claiming
  structural generalization. Remove alternate structures of the same protein,
  homologous templates, and close complexes from the opposite split.
- Record template/search database cutoff. Predicted structures may encode
  training templates that make an apparently novel test protein familiar.
- Mask low-confidence or missing coordinates according to a written policy.
  Keep confidence as provenance, not an unrestricted label proxy.
- Test rigid-transform invariance/equivariance and residue/atom permutation
  behavior. Keep coordinate units and vector targets consistent.

## Variant And DMS Contract

- Identify the exact wild-type sequence, assay, condition, replicate, mutation
  notation, and score normalization. Do not merge fitness scales across assays
  without an explicit calibration model.
- Keep variants from one wild type/assay together for cross-protein claims.
  Separate within-assay mutation interpolation from zero-shot protein transfer.
- Prevent parent leakage: a multi-mutant test sequence must not be reconstructed
  trivially from its single-mutant components when claiming combinatorial
  generalization.
- Define treatment of synonymous, stop, indel, missing, lethal-floor, and
  saturated measurements. Preserve censoring and measurement uncertainty.
- Measure epistasis explicitly for higher-order variants; additive single-site
  baselines are mandatory.

## Evaluation

- Structure/residue tasks: report per-chain/protein metrics and performance by
  sequence identity, fold, confidence, and experimental/predicted source.
- Variant regression/ranking: Spearman plus error/calibration; report within-
  assay and cross-assay separately.
- Design/inverse folding: sequence recovery is not function. Add structural
  consistency, diversity, novelty, and experimental-property proxies with their
  uncertainty.
- Compare sequence-only, nearest-homolog, conservation/profile, and simple
  additive mutation baselines.
